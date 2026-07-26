#!/usr/bin/env bash
#
# Per-library harness for mt-admin-lib/ports-registry.sh (MT-ADMIN-DECOMPOSITION
# §5.1). Exercises the v2..v11 migration ladder, allocation/deallocation, feature
# enablement, and the registry queries against a fixture registry — no root, no
# /etc/lunarwing, no tenant users, no init system.
#
# The library is reached by sourcing the composition root (which defines the
# constants it depends on) with PORTS_REGISTRY redirected at a temp file.
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$TEST_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT
failures=0

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

assert_eq() {  # <label> <actual> <expected>
  if [[ "$2" == "$3" ]]; then
    pass "$1"
  else
    fail "$1 (expected [$3], got [$2])"
  fi
}

assert_jq() {  # <label> <jq-filter> <expected>
  assert_eq "$1" "$(jq -r "$2" "$PORTS_REGISTRY")" "$3"
}

# Silence the migrations' progress chatter; keep die's stderr.
say() { :; }

write_registry() {  # <json>
  PORTS_REGISTRY="$FIXTURE/ports-$RANDOM.json"
  printf '%s\n' "$1" >"$PORTS_REGISTRY"
}

echo "=== ports-registry: migration ladder ==="

# A v1 registry as shipped before the ladder existed: ten numbered reserved_N
# slots, no extended range, no worker selection, no feature flags.
write_registry '{
  "version": 1,
  "range": { "start": 10000, "end": 19999 },
  "block_size": 10,
  "tenants": {
    "legacy": {
      "base_port": 10000,
      "user": "legacy",
      "created_at": "2026-01-01T00:00:00Z",
      "ports": {
        "gateway": 10000, "http": 10001, "bridge": 10002, "postgres": 10003,
        "proxy": 10004, "weechat": 10005,
        "reserved_0": 10006, "reserved_1": 10007,
        "reserved_2": 10008, "reserved_3": 10009
      }
    }
  }
}'

ports_migrate

assert_jq 'v1->v11 lands on version 11' '.version' 11
assert_jq 'v2 renamed reserved_0 -> orchestrator' '.tenants.legacy.ports.orchestrator' 10006
assert_jq 'v3 renamed reserved_1 -> nanocode_wss' '.tenants.legacy.ports.nanocode_wss' 10007
assert_jq 'v4 renamed reserved_2 -> pebble_wss' '.tenants.legacy.ports.pebble_wss' 10008
assert_jq 'v5 renamed reserved_3 -> weechat_adapter' '.tenants.legacy.ports.weechat_adapter' 10009
assert_jq 'no reserved_N slots survive in ports' \
  '[.tenants.legacy.ports | keys[] | select(startswith("reserved_"))] | length' 0
assert_jq 'v6 added the extended range' '.extended_range.start' 20000
assert_jq 'v6 mapped extended_base off base_port' '.tenants.legacy.extended_base' 20000
assert_jq 'v7 assigned darkirc_irc at extended_base+1' \
  '.tenants.legacy.extended_ports.darkirc_irc' 20001
assert_jq 'v7 assigned darkirc_rpc at extended_base+2' \
  '.tenants.legacy.extended_ports.darkirc_rpc' 20002
assert_jq 'v8 dedicated nanocode_health at extended_base+3' \
  '.tenants.legacy.extended_ports.nanocode_health' 20003
assert_jq 'v8 dedicated pebble_health at extended_base+4' \
  '.tenants.legacy.extended_ports.pebble_health' 20004
assert_jq 'v9/v10 dedicated vision_service + vision_health' \
  '"\(.tenants.legacy.extended_ports.vision_service)/\(.tenants.legacy.extended_ports.vision_health)"' \
  '20005/20006'
assert_jq 'v11 assigned opencode_wss from the reserved_7 slot' \
  '.tenants.legacy.extended_ports.opencode_wss' 20007
assert_jq 'v11 assigned opencode_health from the reserved_8 slot' \
  '.tenants.legacy.extended_ports.opencode_health' 20008
# reserved_9 is the one deliberately-unassigned spare in the extended block; every
# other reserved_N must have been consumed by the ladder.
assert_jq 'only reserved_9 remains unassigned in extended_ports' \
  '[.tenants.legacy.extended_ports | keys[] | select(startswith("reserved_"))] | join(",")' \
  'reserved_9'
assert_jq 'darkirc_adapter present after repair path' \
  '(.tenants.legacy.extended_ports | has("darkirc_adapter"))' true

# Re-running the whole ladder must be a no-op: operators re-run add-tenant and
# ports_registry_init on every invocation.
before="$(cat "$PORTS_REGISTRY")"
ports_migrate
assert_eq 'ladder is idempotent on an already-v11 registry' "$(cat "$PORTS_REGISTRY")" "$before"

# An empty v10 registry still gets its version bumped (regression: v11 used to
# leave .version stale when there were no tenants to rewrite).
write_registry '{"version":10,"range":{"start":10000,"end":19999},"block_size":10,"tenants":{}}'
ports_migrate
assert_jq 'empty registry version still bumps to 11' '.version' 11

echo
echo "=== ports-registry: allocation ==="

write_registry '{"version":11,"range":{"start":10000,"end":19999},"block_size":10,
  "extended_range":{"start":20000,"end":29999},"extended_block_size":10,"tenants":{}}'

base_a="$(ports_allocate alpha)"
assert_eq 'first allocation takes the bottom of the range' "$base_a" 10000
base_b="$(ports_allocate bravo)"
assert_eq 'second allocation takes the next block' "$base_b" 10010
assert_eq 'ports_get resolves a core service' "$(ports_get alpha gateway)" 10000
assert_eq 'ports_get resolves an offset core service' "$(ports_get bravo postgres)" 10013
assert_eq 'ports_get resolves an extended service' "$(ports_get bravo vision_service)" 20015
assert_jq 'allocation records the tenant user' '.tenants.bravo.user' bravo
assert_jq 'workers default to unselected' \
  '[.tenants.alpha.workers | to_entries[] | select(.value)] | length' 0
assert_jq 'darkirc defaults to disabled' '.tenants.alpha.enable_darkirc' false

# Re-allocating an existing tenant is the add-tenant resume path: same base, no
# second block consumed.
base_a2="$(ports_allocate alpha 2>/dev/null)"
assert_eq 'reallocation reuses the existing base (resume path)' "$base_a2" "$base_a"
assert_jq 'reallocation did not add a tenant' '.tenants | length' 2

# Feature enablement flips flags in place without moving ports.
ports_enable_darkirc alpha
assert_jq 'ports_enable_darkirc sets the flag' '.tenants.alpha.enable_darkirc' true
assert_eq 'tenant_darkirc_enabled agrees' \
  "$(tenant_darkirc_enabled alpha && echo yes || echo no)" yes
assert_eq 'tenant_darkirc_enabled is false for the other tenant' \
  "$(tenant_darkirc_enabled bravo && echo yes || echo no)" no

ports_enable_proxy alpha
assert_jq 'ports_enable_proxy sets the flag' '.tenants.alpha.enable_proxy' true
assert_eq 'tenant_proxy_enabled agrees' \
  "$(tenant_proxy_enabled alpha && echo yes || echo no)" yes

ports_enable_worker bravo opencode
assert_jq 'ports_enable_worker selects only that worker' \
  '[.tenants.bravo.workers | to_entries[] | select(.value) | .key] | join(",")' opencode
assert_eq 'tenant_worker_enabled agrees for the selected worker' \
  "$(tenant_worker_enabled bravo opencode && echo yes || echo no)" yes
assert_eq 'tenant_worker_enabled is false for an unselected worker' \
  "$(tenant_worker_enabled bravo pebble && echo yes || echo no)" no

echo
echo "=== ports-registry: queries and deallocation ==="

assert_eq 'tenant_exists_in_registry finds a tenant' \
  "$(tenant_exists_in_registry alpha && echo yes || echo no)" yes
assert_eq 'tenant_exists_in_registry rejects an unknown tenant' \
  "$(tenant_exists_in_registry ghost && echo yes || echo no)" no
assert_eq 'all_tenant_names lists every tenant' \
  "$(all_tenant_names | sort | paste -sd, -)" 'alpha,bravo'

ports_deallocate alpha
assert_jq 'deallocation removes the tenant' '(.tenants | has("alpha"))' false
assert_jq 'deallocation leaves other tenants intact' '(.tenants | has("bravo"))' true
assert_eq 'the freed block is handed to the next tenant' "$(ports_allocate charlie)" 10000

release_ports_lock

echo
if [[ "$failures" -eq 0 ]]; then
  echo "RESULT: all ports-registry library tests passed"
  exit 0
fi
echo "RESULT: $failures failure(s)"
exit 1
