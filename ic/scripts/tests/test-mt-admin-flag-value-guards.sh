#!/usr/bin/env bash
# Standalone test harness for the value-flag guards (WI#637).
#
# Before WI#637 a value flag without a value aborted with `$2: unbound
# variable` under `set -u`, and `--flag --next-flag` silently consumed the
# next flag as the value and exited 0 — e.g. `add-tenant probe --llm-api-key
# --with-kimi` wrote LLM_API_KEY=--with-kimi and left the worker disabled.
# `need_value "$@"` (mt-admin-lib/00-common.sh, plus the standalone copy in
# import-tenant.sh) closes both shapes. It refuses a value only when the value
# has the shape of a flag, and it never prints the value of a secret flag.
#
# Sources lunarwing-mt-admin.sh (its BASH_SOURCE guard skips main()) and
# drives the real dispatcher with the root/lock/runtime edges stubbed, so the
# assertions cover the actual `case` arms rather than a copy of them.
# import-tenant.sh is run as a command without a bundle: it parses its flags
# and stops at its usage line before it looks at the host.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
IMPORT_SCRIPT="$SCRIPT_DIR/../import-tenant.sh"
COMMON_LIB="$SCRIPT_DIR/../mt-admin-lib/00-common.sh"

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
assert_not_contains() {  # <label> <haystack> <needle>
  if [[ "$2" != *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        expected not to contain: [$3]"
    echo "        actual:                  [$2]"
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
assert_contains "flag-as-value says why" "$out" "cannot look like a flag"

rc=0; need_value --some-flag "value" || rc=$?
assert_eq "plain value accepted" "$rc" "0"

rc=0; need_value --some-flag "" || rc=$?
assert_eq "empty value still accepted" "$rc" "0"

rc=0; need_value --expires-in-days "-1" || rc=$?
assert_eq "single-dash value still accepted" "$rc" "0"

echo "=== need_value helper: only a flag-shaped value is refused ==="
# Two dashes, a lowercase letter or digit, then lowercase letters, digits and
# hyphens, with or without =value: that is every long flag of the tool, and
# nothing else is refused.
for value in "--Zq9examplePW" "-- alerts --" "--" "-" "--UPPER" "--Mixed-case" \
  "--with_underscore" "--has space" "--dot.ted" "---three-dashes" "--=x" \
  "--Zq9=example" "--has space=1"; do
  rc=0; ( need_value --some-flag "$value" ) >/dev/null 2>&1 || rc=$?
  assert_eq "value [$value] is accepted" "$rc" "0"
done
for value in "--with-kimi" "--a" "--9" "--model" "--opencode2-model" \
  "--target=main" "--sandbox-image=never" "--selected-commit="; do
  rc=0; out="$( need_value --some-flag "$value" 2>&1 )" || rc=$?
  assert_eq "flag-shaped value [$value] is refused" "$rc" "1"
done

echo "=== need_value helper: the value of a secret flag is never printed ==="
for flag in --xmpp-password --llm-api-key --nanogpt-api-key; do
  rc=0; out="$( need_value "$flag" --looks-like-a-flag 2>&1 )" || rc=$?
  assert_eq "$flag with a flag-shaped value rc" "$rc" "1"
  assert_contains "$flag message" "$out" "$flag requires a value"
  assert_contains "$flag says why" "$out" "cannot look like a flag"
  assert_not_contains "$flag does not print the value" "$out" "--looks-like-a-flag"
  rc=0; ( need_value "$flag" "--Zq9examplePW" ) >/dev/null 2>&1 || rc=$?
  assert_eq "$flag accepts a secret that only begins with dashes" "$rc" "0"
done

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
run_denied "configure-pebble --nanogpt-api-key" --nanogpt-api-key configure-pebble probeA

echo "=== dispatcher: a neighbouring flag is not swallowed as the value ==="
run_swallow_denied() {  # <label> <flag> <next-flag> <command...>
  local label="$1" flag="$2" next="$3" out rc=0
  shift 3
  out="$( main "$@" "$flag" "$next" 2>&1 )" || rc=$?
  assert_eq "$label rc" "$rc" "1"
  assert_contains "$label message" "$out" "$flag requires a value"
  # Only the name is shown: what follows an equals sign may be a value the
  # operator did not mean to print.
  assert_contains "$label names the swallowed flag" "$out" "(found: ${next%%=*})"
  if [[ "$next" == *=?* ]]; then
    assert_eq "$label keeps the =value part out of the message" \
      "$(grep -cF -- "${next#*=}" <<<"$out" || true)" "0"
  fi
}
# The same refusal after a flag whose value is a secret: the value stays out
# of the output, because a real secret could have that shape too.
run_swallow_hidden() {  # <label> <flag> <next-flag> <command...>
  local label="$1" flag="$2" next="$3" out rc=0
  shift 3
  out="$( main "$@" "$flag" "$next" 2>&1 )" || rc=$?
  assert_eq "$label rc" "$rc" "1"
  assert_contains "$label message" "$out" "$flag requires a value"
  assert_not_contains "$label does not print the value" "$out" "$next"
}

run_swallow_denied "add-tenant --gateway-host --with-kimi" --gateway-host --with-kimi add-tenant probeA
run_swallow_denied "add-tenant --lunardepth-model --with-lunardepth" --lunardepth-model --with-lunardepth add-tenant probeA
run_swallow_denied "configure-voice --stt-engine --disable-vad" --stt-engine --disable-vad configure-voice probeA
run_swallow_denied "build-worker-candidates --target --selected-commit" --target --selected-commit build-worker-candidates probeA
# build-worker-candidates and upgrade-tenant also take --name=value forms; one
# of those after a forgotten value is a swallowed flag as well.
run_swallow_denied "build-worker-candidates --target --selected-commit=abc" --target --selected-commit=abc build-worker-candidates probeA
run_swallow_denied "build-worker-candidates --selected-commit --sandbox-image=never" --selected-commit --sandbox-image=never build-worker-candidates probeA
run_swallow_hidden "add-tenant --llm-api-key --with-kimi" --llm-api-key --with-kimi add-tenant probeA
run_swallow_hidden "add-tenant --xmpp-password --with-kimi" --xmpp-password --with-kimi add-tenant probeA
run_swallow_hidden "add-tenants --llm-api-key --with-kimi" --llm-api-key --with-kimi add-tenants probeA
run_swallow_hidden "configure-pebble --nanogpt-api-key --model" --nanogpt-api-key --model configure-pebble probeA

echo "=== dispatcher: valid values keep their arm (no regression) ==="
# Shellcheck cannot follow the dispatcher's positional contract; the indices
# are add_tenant's, and WI#637's census used the same ones (arg 4, arg 8,
# arg 13, arg 19).
# shellcheck disable=SC2317
add_tenant() {
  printf 'xmpp_password=%s\nllm_api_key=%s\ngateway_host=%s\nwith_kimi=%s\n' "$4" "$8" "${13}" "${19}"
}
# shellcheck disable=SC2317
require_tenant_llm_endpoint() { :; }
# shellcheck disable=SC2317
with_tenant_lifecycle_lock() { local _tenant="$1"; shift; "$@"; }
sanitize_name() { printf '%s' "$1"; }

out="$( main add-tenant probeA --llm-api-key sekrit --with-kimi 2>&1 )"
assert_contains "llm-api-key value reaches add_tenant" "$out" "llm_api_key=sekrit"
assert_contains "follow-on --with-kimi still applies" "$out" "with_kimi=true"

# A secret may begin with two dashes. It is not flag-shaped, so it is a value.
out="$( main add-tenant probeA --xmpp-password '--Zq9examplePW' --llm-api-key '--Zq9exampleKEY' --with-kimi 2>&1 )" || true
assert_contains "a password that begins with dashes reaches add_tenant" "$out" "xmpp_password=--Zq9examplePW"
assert_contains "an API key that begins with dashes reaches add_tenant" "$out" "llm_api_key=--Zq9exampleKEY"
assert_contains "the flag after those values still applies" "$out" "with_kimi=true"

out="$( main add-tenant probeA --llm-api-key k --gateway-host '' 2>&1 )" || true
assert_contains "an empty value reaches add_tenant as empty" "$out" "gateway_host="$'\n'

echo "=== import-tenant.sh: the same guard on its own parser ==="
# No bundle is named in any of these runs, so a parse that succeeds ends at
# the script's usage line.
run_import() {  # <args...> -> import_out, import_rc
  import_rc=0
  import_out="$( bash "$IMPORT_SCRIPT" "$@" 2>&1 )" || import_rc=$?
}

for flag in --name --lunardepth-model --lunardepth-base-url --lunardepth-api-key-file --owner-scope; do
  run_import "$flag"
  assert_eq "import-tenant $flag without a value rc" "$import_rc" "1"
  assert_contains "import-tenant $flag without a value message" "$import_out" "$flag requires a value"
  run_import "$flag" --dry-run
  assert_eq "import-tenant $flag --dry-run rc" "$import_rc" "1"
  assert_contains "import-tenant $flag --dry-run message" "$import_out" "$flag requires a value"
  assert_contains "import-tenant $flag --dry-run names the swallowed flag" "$import_out" "--dry-run"
done

run_import --name tenant-a --owner-scope 0123456789abcdef0123456789abcdef \
  --lunardepth-model some-model --lunardepth-base-url http://127.0.0.1:1/v1 --dry-run
assert_eq "import-tenant with values and no bundle rc" "$import_rc" "1"
assert_contains "import-tenant with values stops at its usage line" "$import_out" "usage:"
assert_not_contains "import-tenant with values is not refused by the guard" "$import_out" "requires a value"

run_import --name '--Odd-Name'
assert_contains "import-tenant accepts a value that only begins with dashes" "$import_out" "usage:"
assert_not_contains "import-tenant does not refuse that value" "$import_out" "requires a value"

echo "=== the two copies of need_value are the same ==="
function_body() {  # <file> <name> -> the function, from "name() {" to the "}" in column 0
  awk -v name="$2" '
    $0 == name "() {" { on = 1 }
    on { print }
    on && $0 == "}" { exit }
  ' "$1"
}
library_copy="$(function_body "$COMMON_LIB" need_value)"
import_copy="$(function_body "$IMPORT_SCRIPT" need_value)"
assert_contains "the library's need_value was found" "$library_copy" "cannot look like a flag"
assert_eq "import-tenant.sh carries the library's need_value unchanged" "$import_copy" "$library_copy"

echo "=== structural: every value flag carries need_value ==="
# A value flag is consumed by `shift 2`. The check reports a `shift 2` that no
# `need_value` precedes, in the two places where a flag's value is consumed:
#   - inside a case arm opened by a flag pattern (`--name)`, `--yes|-y)`), up
#     to the arm's `;;`;
#   - anywhere else inside a `while [[ $# -gt 0 ]]` parse loop, since the last
#     `;;` or `esac` (the shared value path of configure-voice).
# A positional `shift 2` outside both, such as a helper that reads "$1" and
# "$2", is not a value flag and is not reported. The --in arm of
# darkirc-contact validate-migration is the one intended exception: it accepts
# only a literal '-' and checks that itself.
# With --count the function prints how many value shifts it examined, so the
# callers below can tell a clean file from one the check could not read.
# The arm pattern is one bracket expression on purpose: mawk 1.3.4, the awk of
# the CI image, does not match a starred group that repeats zero times after
# another star, and the self-test below is what showed it.
unguarded_value_shifts() {  # [--count] <file>
  local mode=report
  if [[ "$1" == --count ]]; then mode=count; shift; fi
  awk -v mode="$mode" '
    function indent(s) { match(s, /^[ \t]*/); return RLENGTH }
    /^[ \t]*#/ { next }
    {
      if (!in_loop && $0 ~ /while \[\[ \$# -gt 0 \]\]; do/) { in_loop = 1; loop_indent = indent($0) }
      if ($0 ~ /^[ \t]*-[-a-zA-Z0-9*|=]*\)/) { in_arm = 1; need = 0 }
      if ($0 ~ /need_value/) need = 1
      if ($0 ~ /shift 2/ && (in_arm || in_loop) && $0 !~ /--in\)/) {
        examined++
        if (!need && mode == "report") printf "%s:%d: %s\n", FILENAME, FNR, $0
      }
      if ($0 ~ /;;/ || $0 ~ /^[ \t]*esac/) { in_arm = 0; need = 0 }
      if (in_loop && $0 ~ /^[ \t]*done/ && indent($0) == loop_indent) { in_loop = 0; need = 0 }
    }
    END { if (mode == "count") print examined + 0 }
  ' "$1"
}

# The check has to prove itself first: a first version of it never matched a
# line and passed on any input. The fixture marks every line that must be
# reported, and nothing else may be.
fixture_dir="$(mktemp -d)"
cat >"$fixture_dir/fixture.sh" <<'FIXTURE'
helper() {
  local first="$1" second="$2"
  shift 2
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --guarded) need_value "$@"; GUARDED="$2"; shift 2 ;;
    --unguarded) UNGUARDED="$2"; shift 2 ;; # REPORT
    --multi-guarded)
      need_value "$@"
      MULTI="$2"; shift 2
      ;;
    --multi-unguarded)
      MULTI="$2"; shift 2 # REPORT
      ;;
    --pair|-p) PAIR="$2"; shift 2 ;; # REPORT
    --in) [[ $# -ge 2 && "$2" == - ]] || exit 1; shift 2 ;;
    --switch) SWITCH=true; shift ;;
    --inline=*) INLINE="${1#*=}"; shift ;;
    --key-a) KEY=A ;;
    # need_value in a comment does not guard the next arm
    --after-comment) AFTER="$2"; shift 2 ;; # REPORT
    -*) exit 2 ;;
    *) exit 2 ;;
  esac
  VALUE="$2"
  shift 2 # REPORT
done
while [[ $# -gt 0 ]]; do
  case "$1" in
    --key-b) KEY=B ;;
    *) exit 2 ;;
  esac
  need_value "$@"
  VALUE="$2"
  shift 2
done
after_the_loops() {
  shift 2
}
case "$1" in
  --outside-a-loop) OUTSIDE="$2"; shift 2 ;; # REPORT
  --outside-guarded) need_value "$@"; OUTSIDE="$2"; shift 2 ;;
esac
FIXTURE
expected_lines="$(grep -n '# REPORT' "$fixture_dir/fixture.sh" | cut -d: -f1 | tr '\n' ' ')"
reported_lines="$(unguarded_value_shifts "$fixture_dir/fixture.sh" | cut -d: -f2 | tr '\n' ' ')"
examined_in_fixture="$(unguarded_value_shifts --count "$fixture_dir/fixture.sh")"
rm -rf "$fixture_dir"
assert_eq "self-test: the check reports the six unguarded value shifts of its fixture, and only those" \
  "$reported_lines" "$expected_lines"
assert_eq "self-test: the fixture marks six lines" "$(wc -w <<<"$expected_lines" | tr -d ' ')" "6"
assert_eq "self-test: the check examined ten value shifts (not the two positional ones, not --in)" \
  "$examined_in_fixture" "10"

for script in "$ADMIN_SCRIPT" "$IMPORT_SCRIPT"; do
  if bad="$(unguarded_value_shifts "$script")" && [[ -z "$bad" ]]; then
    echo "  PASS: $(basename "$script") has no unguarded value flag"
  else
    echo "  FAIL: $(basename "$script") has unguarded value flags:"
    printf '%s\n' "$bad"
    failures=$((failures + 1))
  fi
  # A clean result means something only if the check saw the value flags: it
  # must have examined at least one `shift 2` per need_value call.
  guard_calls="$(grep -c -E '^[^#]*need_value "\$@"' "$script" || true)"
  examined="$(unguarded_value_shifts --count "$script")"
  if [[ "$guard_calls" -ge 1 && "$examined" -ge "$guard_calls" ]]; then
    echo "  PASS: $(basename "$script"): $examined value shifts examined for $guard_calls need_value calls"
  else
    echo "  FAIL: $(basename "$script"): only $examined value shifts examined for $guard_calls need_value calls;"
    echo "        the check does not recognise a value flag's shape: extend unguarded_value_shifts"
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
