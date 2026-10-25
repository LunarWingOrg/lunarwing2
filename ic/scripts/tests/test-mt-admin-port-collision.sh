#!/usr/bin/env bash
# Regression tests for work item #65: mt-admin port collision warnings.
#
# Covers:
#   - ports_registry_duplicate_ports: detects ports assigned to more than one
#     slot across the registry (cross-tenant, within-tenant, and the
#     host-global health_metrics slot); ignores non-numeric hand edits.
#   - ports_warn_block_in_use: warns (stderr, non-fatal) when a port assigned
#     to a tenant in the registry is already accepting connections on
#     127.0.0.1; silent when ports are free; skips non-numeric values.
#   - ports_allocate integration: stdout stays the bare base port while the
#     collision warning lands on stderr.
#
# Sources lunarwing-mt-admin.sh (its BASH_SOURCE guard skips main() when
# sourced) and runs entirely against fixture registries in a mktemp dir.
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
assert_contains() {  # <label> <haystack> <needle>
  case "$2" in
    *"$3"*) echo "  PASS: $1" ;;
    *)
      echo "  FAIL: $1"
      echo "        missing: [$3]"
      echo "        in:      [$2]"
      failures=$((failures + 1))
      ;;
  esac
}

FIXTURE="$(mktemp -d)"
HOLDER_PIDS=()
cleanup() {
  if [[ ${#HOLDER_PIDS[@]} -gt 0 ]]; then
    kill "${HOLDER_PIDS[@]}" 2>/dev/null || true
  fi
  rm -rf "$FIXTURE"
}
trap cleanup EXIT

PORTS_REGISTRY="$FIXTURE/ports.json"

# Bind 127.0.0.1:<port> in the background and wait until it accepts.
start_holder() {  # <port>
  local port="$1"
  python3 - "$port" <<'PY' &
import socket, sys, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(1)
time.sleep(120)
PY
  HOLDER_PIDS+=($!)
  local _
  for _ in $(seq 1 50); do
    _port_in_use "$port" && return 0
    sleep 0.1
  done
  echo "  FAIL: holder failed to bind 127.0.0.1:$port"
  failures=$((failures + 1))
  return 1
}

# Print an ephemeral loopback port that is free right now.
free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

echo "=== ports_registry_duplicate_ports tests ==="

cat >"$PORTS_REGISTRY" <<'JSON'
{
  "version": 16,
  "health_metrics_port": 50000,
  "tenants": {
    "alpha": {
      "base_port": 51000,
      "ports": {"gateway": 51000, "http": 51001},
      "extended_ports": {"darkirc_adapter": 52000, "vision_health": 52006},
      "kimi_ports": {"kimi_wss": 53000, "kimi_health": 53001},
      "voice_ports": {"voice_service": 54000, "voice_health": 54001}
    },
    "beta": {
      "base_port": 51010,
      "ports": {"gateway": 51010, "http": 51011},
      "extended_ports": {"darkirc_adapter": 52010},
      "kimi_ports": {"kimi_wss": 53010},
      "voice_ports": {"voice_service": 54010}
    }
  }
}
JSON
cp "$PORTS_REGISTRY" "$FIXTURE/clean.json"

assert_eq "clean registry reports no duplicates" \
  "$(ports_registry_duplicate_ports)" ""

# Cross-tenant duplicate: beta.http takes alpha.http's port.
jq '.tenants.beta.ports.http = 51001' "$FIXTURE/clean.json" >"$PORTS_REGISTRY"
assert_eq "cross-tenant duplicate detected" \
  "$(ports_registry_duplicate_ports)" "51001"

# Within-tenant duplicate: alpha.gateway takes alpha.http's port.
jq '.tenants.alpha.ports.gateway = 51001' "$FIXTURE/clean.json" >"$PORTS_REGISTRY"
assert_eq "within-tenant duplicate detected" \
  "$(ports_registry_duplicate_ports)" "51001"

# Host-global health_metrics slot colliding with a tenant port.
jq '.health_metrics_port = 53010' "$FIXTURE/clean.json" >"$PORTS_REGISTRY"
assert_eq "health_metrics collision detected" \
  "$(ports_registry_duplicate_ports)" "53010"

# Hand-edited non-numeric values must be ignored, not crash the query.
jq '.tenants.alpha.ports.weird = "oops"' "$FIXTURE/clean.json" >"$PORTS_REGISTRY"
assert_eq "non-numeric values ignored" \
  "$(ports_registry_duplicate_ports)" ""

# NY-9 (!1907 review): a genuinely unparseable registry (a tenant's .ports is
# a string) must surface as a non-zero status, not a silent "no duplicates".
# Before the fix the helper ended in '|| true' and returned rc=0 with empty
# output, so doctor printed '[PASS] no duplicate ports' for a file it could
# not read. Capture the status explicitly — assert_eq's $(...) would abort
# under set -e on the non-zero return we are asserting for.
jq '.tenants.alpha.ports = "corrupt"' "$FIXTURE/clean.json" >"$PORTS_REGISTRY"
dup_rc=0
dup_out="$(ports_registry_duplicate_ports)" || dup_rc=$?
assert_eq "unparseable registry returns non-zero status" "$dup_rc" "5"
assert_eq "unparseable registry yields empty collision output" "$dup_out" ""

echo "=== ports_warn_block_in_use tests ==="

BUSY_PORT="$(free_port)"
start_holder "$BUSY_PORT"
FREE1="$(free_port)"
FREE2="$(free_port)"

cat >"$PORTS_REGISTRY" <<JSON
{"version":16,"tenants":{
  "collide":{"base_port":51000,"ports":{"gateway":$BUSY_PORT,"http":$FREE1}},
  "clean":{"base_port":51010,"ports":{"gateway":$FREE2,"http":$FREE1}},
  "weird":{"base_port":51020,"ports":{"gateway":"not-a-port"}}
}}
JSON

warn_out="$(ports_warn_block_in_use collide 2>&1)"
assert_contains "bound port triggers collision warning" \
  "$warn_out" "WARNING: port collision for tenant 'collide'"
assert_contains "warning names the colliding slot and port" \
  "$warn_out" "gateway=$BUSY_PORT"
# Negative assertion: free ports must not be named (assert_contains is
# positive-only, so spell it out).
case "$warn_out" in
  *"http=$FREE1"*)
    echo "  FAIL: warning names a free port ($FREE1)"
    echo "        in: [$warn_out]"
    failures=$((failures + 1))
    ;;
  *) echo "  PASS: warning does not name free ports" ;;
esac

assert_eq "all ports free -> no warning" \
  "$(ports_warn_block_in_use clean 2>&1)" ""
assert_eq "non-numeric port skipped silently" \
  "$(ports_warn_block_in_use weird 2>&1)" ""
assert_eq "unknown tenant -> no warning" \
  "$(ports_warn_block_in_use nosuchtenant 2>&1)" ""

echo "=== ports_allocate integration test ==="

cat >"$PORTS_REGISTRY" <<'JSON'
{"version":16,"range":{"start":10000,"end":19999},"block_size":10,
 "extended_range":{"start":20000,"end":29999},"extended_block_size":10,
 "kimi_range":{"start":30000,"end":39999},"kimi_block_size":2,
 "voice_range":{"start":40000,"end":49999},"voice_block_size":2,
 "health_metrics_port":50000,"tenants":{}}
JSON

# ports_allocate picks the first free base block from the global constants
# (10000-19999), so hold a port inside block 10000-10009 if one is free.
BLOCK_PORT=""
for p in 10004 10006 10007 10008 10005 10009; do
  if ! _port_in_use "$p"; then BLOCK_PORT="$p"; break; fi
done

if [[ -z "$BLOCK_PORT" ]]; then
  echo "  SKIP: no bindable port in 10000-10009 on this host"
else
  start_holder "$BLOCK_PORT"
  alloc_out="$(ports_allocate integ false false '{}' '' 2>"$FIXTURE/alloc.err")"
  assert_eq "ports_allocate stdout is the bare base port" "$alloc_out" "10000"
  assert_contains "ports_allocate warns about the held port on stderr" \
    "$(cat "$FIXTURE/alloc.err")" "$BLOCK_PORT"
  assert_eq "registry recorded the new tenant" \
    "$(jq -r '.tenants.integ.base_port' "$PORTS_REGISTRY")" "10000"
fi

echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
