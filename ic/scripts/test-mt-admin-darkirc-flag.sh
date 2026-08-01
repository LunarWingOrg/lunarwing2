#!/usr/bin/env bash
# Regression test for H2: `--enable-darkirc` must be flippable on an existing
# tenant. ports_allocate()'s resume path reconciles the registry flag so the
# env/unit writers (which gate on tenant_darkirc_enabled, a registry read) stay
# consistent with add_tenant()'s in-memory flag.
#
# Sources lunarwing-mt-admin.sh's functions into an isolated temp registry and
# exercises ports_allocate / ports_enable_darkirc WITHOUT touching /etc/lunarwing
# or any real tenant. Does NOT require root.
#
# Exits: 0 PASS / 1 FAIL / 2 precondition error.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/lunarwing-mt-admin.sh"

[[ -f "$MT_ADMIN" ]] || { echo "error: $MT_ADMIN not found" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "error: jq required" >&2; exit 2; }

# Source the admin script — functions only; `main` is guarded by the
# BASH_SOURCE[0] == $0 check at the bottom of the file, so sourcing is safe.
# shellcheck source=lunarwing-mt-admin.sh
source "$MT_ADMIN"

# Redirect the isolated registry AWAY from /etc/lunarwing/ports.json.
PORTS_REGISTRY="$(mktemp -t mt-darkirc-flag.XXXXXX.json)"
trap 'rm -f "$PORTS_REGISTRY"' EXIT

# Seed an empty v7 registry (what ports_registry_init would write) so this test
# never calls ports_registry_init (which mkdir's /etc/lunarwing).
cat >"$PORTS_REGISTRY" <<'ENDJSON'
{
  "version": 7,
  "range": { "start": 10000, "end": 19999 },
  "block_size": 10,
  "extended_range": { "start": 20000 },
  "extended_block_size": 10,
  "tenants": {}
}
ENDJSON

PASS=0
FAIL=0

assert_eq() {
  local desc="$1" got="$2" want="$3"
  if [[ "$got" == "$want" ]]; then
    echo "  ok   - $desc"
    PASS=$((PASS + 1))
  else
    echo "  FAIL - $desc (got: '$got', want: '$want')"
    FAIL=$((FAIL + 1))
  fi
}

darkirc_flag() {
  # NOTE: jq's `//` treats `false` like `null`, so `false // "x"` == "x". Use an
  # explicit null-check to distinguish false from a genuinely missing key.
  jq -r ".tenants[\"$1\"].enable_darkirc | if . == null then \"<missing>\" else tostring end" "$PORTS_REGISTRY"
}

darkirc_scope() {
  jq -r ".tenants[\"$1\"].darkirc_scope_id // \"<missing>\"" "$PORTS_REGISTRY"
}

assert_scope_id() {
  local desc="$1" scope_id="$2"
  if [[ "$scope_id" =~ ^[0-9a-f]{32}$ ]]; then
    echo "  ok   - $desc"
    PASS=$((PASS + 1))
  else
    echo "  FAIL - $desc (got: '$scope_id')"
    FAIL=$((FAIL + 1))
  fi
}

echo "== H2: --enable-darkirc flag flip on existing tenant =="

# 1. Fresh tenant, darkirc disabled.
base1="$(ports_allocate alpha false 2>/dev/null)"
assert_eq "fresh tenant base_port (alpha)"        "$base1"               "10000"
assert_eq "alpha enable_darkirc initially false"  "$(darkirc_flag alpha)" "false"

# 2. Resume the SAME tenant WITH darkirc — must reuse base AND flip the flag (H2).
base1b="$(ports_allocate alpha true 2>/dev/null)"
assert_eq "resume reuses base_port (alpha)"       "$base1b"               "10000"
assert_eq "alpha enable_darkirc flips to true"    "$(darkirc_flag alpha)" "true"
alpha_scope="$(darkirc_scope alpha)"
assert_scope_id "enabling darkirc assigns scope ID" "$alpha_scope"

# 3. Resume WITHOUT the flag must NOT silently downgrade an enabled tenant
#    (reconciliation is one-directional; disabling is a manual teardown).
base1c="$(ports_allocate alpha false 2>/dev/null)"
assert_eq "resume w/o flag keeps base_port (alpha)" "$base1c"               "10000"
assert_eq "alpha stays enabled (no silent downgrade)" "$(darkirc_flag alpha)" "true"
assert_eq "resume preserves alpha scope ID" "$(darkirc_scope alpha)" "$alpha_scope"

# 4. A second fresh tenant with darkirc enabled from the start.
base2="$(ports_allocate beta true 2>/dev/null)"
assert_eq "second tenant base_port (beta)"        "$base2"               "10010"
assert_eq "beta enable_darkirc true on create"    "$(darkirc_flag beta)" "true"
beta_scope="$(darkirc_scope beta)"
assert_scope_id "fresh darkirc tenant gets scope ID" "$beta_scope"
scope_distinct="duplicate"
[[ "$alpha_scope" != "$beta_scope" ]] && scope_distinct="distinct"
assert_eq "tenant scope IDs are distinct" "$scope_distinct" "distinct"

# 5. ports_enable_darkirc is idempotent on an already-enabled tenant (no rewrite).
before_mtime="$(stat -c %Y "$PORTS_REGISTRY" 2>/dev/null || stat -f %m "$PORTS_REGISTRY")"
ports_enable_darkirc beta 2>/dev/null || true
after_mtime="$(stat -c %Y "$PORTS_REGISTRY" 2>/dev/null || stat -f %m "$PORTS_REGISTRY")"
assert_eq "idempotent: registry not rewritten"     "$after_mtime"          "$before_mtime"
assert_eq "beta still enabled after idempotent call" "$(darkirc_flag beta)" "true"
assert_eq "idempotent enable preserves beta scope ID" "$(darkirc_scope beta)" "$beta_scope"

# 6. Structured migration can preserve a source scope, but never duplicate it.
import_scope="fedcba9876543210fedcba9876543210"
ports_allocate imported true false '{}' "$import_scope" >/dev/null 2>&1
assert_eq "import preserves requested DarkIRC scope" "$(darkirc_scope imported)" "$import_scope"
duplicate_import_result="rejected"
if (ports_allocate duplicate-import true false '{}' "$import_scope" >/dev/null 2>&1); then
  duplicate_import_result="accepted"
fi
assert_eq "import rejects duplicate DarkIRC scope" "$duplicate_import_result" "rejected"

# 7. Separate mt-admin processes must serialize allocation against the same
#    registry instead of selecting the same free block from stale snapshots.
parallel_dir="$(mktemp -d -t mt-port-lock.XXXXXX)"
bash -c 'source "$1"; PORTS_REGISTRY="$2"; ports_allocate gamma false' \
  _ "$MT_ADMIN" "$PORTS_REGISTRY" >"$parallel_dir/gamma" &
gamma_pid=$!
bash -c 'source "$1"; PORTS_REGISTRY="$2"; ports_allocate delta false' \
  _ "$MT_ADMIN" "$PORTS_REGISTRY" >"$parallel_dir/delta" &
delta_pid=$!
wait "$gamma_pid" "$delta_pid"
gamma_base="$(<"$parallel_dir/gamma")"
delta_base="$(<"$parallel_dir/delta")"
rm -rf "$parallel_dir"
parallel_result="duplicate"
[[ "$gamma_base" != "$delta_base" ]] && parallel_result="distinct"
assert_eq "parallel allocations receive distinct port blocks" "$parallel_result" "distinct"

# 8. A v11 registry can still need the versionless v6.1 repair. That write must
#    be protected even though the registry's numeric version is current.
cat >"$PORTS_REGISTRY" <<'ENDJSON'
{
  "version": 11,
  "range": { "start": 10000, "end": 19999 },
  "block_size": 10,
  "extended_range": { "start": 20000, "end": 29999 },
  "extended_block_size": 10,
  "tenants": {
    "legacy": {
      "extended_ports": { "reserved_0": 20000 }
    }
  }
}
ENDJSON
LOCK_CALLS=0
acquire_ports_lock() {
  LOCK_CALLS=$((LOCK_CALLS + 1))
  PORTS_LOCK_HELD="true"
}
release_ports_lock() { PORTS_LOCK_HELD="false"; }
ports_registry_init 2>/dev/null
assert_eq "v11 repair acquires registry lock" "$LOCK_CALLS" "1"
assert_eq "v11 repair assigns darkirc adapter" \
  "$(jq -r '.tenants.legacy.extended_ports.darkirc_adapter' "$PORTS_REGISTRY")" "20000"

# 8. v13 -> v14 creates missing scope IDs, preserves valid IDs, skips disabled
# tenants, and rejects duplicate identities instead of silently replacing them.
known_scope="0123456789abcdef0123456789abcdef"
cat >"$PORTS_REGISTRY" <<JSON
{
  "version": 13,
  "tenants": {
    "missing": {"enable_darkirc": true},
    "known": {"enable_darkirc": true, "darkirc_scope_id": "$known_scope"},
    "disabled": {"enable_darkirc": false}
  }
}
JSON
ports_migrate_v14 2>/dev/null
assert_eq "v14 migration bumps registry version" "$(jq -r '.version' "$PORTS_REGISTRY")" "14"
assert_scope_id "v14 migration assigns missing scope ID" "$(darkirc_scope missing)"
assert_eq "v14 migration preserves valid scope ID" "$(darkirc_scope known)" "$known_scope"
assert_eq "v14 migration skips disabled tenant" "$(darkirc_scope disabled)" "<missing>"

jq --arg scope "$known_scope" \
  '.version = 13 | .tenants.duplicate = {enable_darkirc: true, darkirc_scope_id: $scope}' \
  "$PORTS_REGISTRY" >"$PORTS_REGISTRY.tmp"
mv "$PORTS_REGISTRY.tmp" "$PORTS_REGISTRY"
duplicate_before="$(sha256sum "$PORTS_REGISTRY" | cut -d' ' -f1)"
duplicate_result="accepted"
if (ports_migrate_v14 >/dev/null 2>&1); then
  duplicate_result="accepted"
else
  duplicate_result="rejected"
fi
assert_eq "v14 migration rejects duplicate scope IDs" "$duplicate_result" "rejected"
assert_eq "failed v14 migration leaves version unchanged" "$(jq -r '.version' "$PORTS_REGISTRY")" "13"
assert_eq "failed v14 migration is byte-atomic" \
  "$(sha256sum "$PORTS_REGISTRY" | cut -d' ' -f1)" "$duplicate_before"

jq '.tenants.duplicate.darkirc_scope_id = "00000000000000000000000000000000"' \
  "$PORTS_REGISTRY" >"$PORTS_REGISTRY.tmp"
mv "$PORTS_REGISTRY.tmp" "$PORTS_REGISTRY"
zero_scope_result="accepted"
if (ensure_darkirc_scope_id duplicate >/dev/null 2>&1); then
  zero_scope_result="accepted"
else
  zero_scope_result="rejected"
fi
assert_eq "all-zero scope ID is rejected" "$zero_scope_result" "rejected"

echo ""
if [[ "$FAIL" -eq 0 ]]; then
  echo "RESULT: PASS ($PASS assertions)"
  exit 0
else
  echo "RESULT: FAIL ($PASS passed, $FAIL failed)"
  exit 1
fi
