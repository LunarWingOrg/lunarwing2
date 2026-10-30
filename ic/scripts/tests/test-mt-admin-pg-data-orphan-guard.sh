#!/usr/bin/env bash
# item-100 run-280 RZ-3: pin the PG rootless-data / root-store-container guard.
# _rootless_pg_data_present and _root_store_pg_container_present must be probed
# with the exact semantics the guard depends on:
#   - the rootless volume probe compares {{.Name}} for EQUALITY, because
#     `volume inspect <name>` prefix-matches a longer volume
#     (lunarwing-pg-<t> resolves to lunarwing-pg-<t>2);
#   - the root-store probe uses the TYPED `container inspect`, because a generic
#     inspect would cross-match a root-store VOLUME named lunarwing-pg-<t>-...
#     (run-280 RZ-2) and fire the guard on a container that does not exist.
# The guard decision itself is exercised through start_tenant_postgres in a
# CHILD bash (die() exits its shell by design): it must die exactly in the
# (rootless data absent + root container present) window and be silenced by the
# ack env or by the exact-name volume existing.
# Mutation targets: replacing the {{.Name}} equality with a non-empty test, or
# reverting the typed probe to a generic inspect, must each fail a case below.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0
pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*"; failures=$((failures + 1)); }

# ── fixture store (filesystem-backed so the child bash sees the same state) ──
#   tenant (rootless) store: containers/ and volumes/ under $fixture/rootless;
#   root store: containers/ and volumes/ under $fixture/root.
# The child sources $fixture/store.sh, which defines _ctr/_root_ctr over these
# directories. A volume ONLY counts when its EXACT name is passed (the prefix
# cross-match is the production podman behavior under test), and the root-store
# generic-inspect arm resolves ACROSS object types (the RZ-2 hazard).
mkdir -p "$fixture/rootless/containers" "$fixture/root/containers" \
  "$fixture/rootless/volumes" "$fixture/root/volumes"

cat >"$fixture/store.sh" <<'STORE'
_ctr() {
  local tenant="$1"; shift
  case "${1:-}" in
    container)
      [[ "${2:-}" == inspect ]] || return 0
      local probe="${3:-}"; [[ "${probe:0:1}" == - ]] && probe="$4"
      [[ -e "$FIXTURE_ROOT/rootless/containers/$probe" ]] ;;
    volume)
      [[ "${2:-}" == inspect ]] || return 0
      local name="${@: -1}" v
      # production podman: volume inspect PREFIX-MATCHES, returning the LONGER
      # volume for lunarwing-pg-<t> when lunarwing-pg-<t>2 exists
      for v in "$FIXTURE_ROOT/rootless/volumes/"*; do
        [[ -e "$v" ]] || continue
        if [[ "${v##*/}" == "$name"* ]]; then printf '%s\n' "${v##*/}"; return 0; fi
      done
      return 1 ;;
    *) return 0 ;;
  esac
}
_root_ctr() {
  case "${1:-}" in
    container)
      [[ "${2:-}" == inspect ]] || return 0
      [[ -e "$FIXTURE_ROOT/root/containers/$3" ]] ;;
    volume)
      # RZ-2: a GENERIC inspect in the root store resolves across object types,
      # so a lunarwing-pg-<t>... VOLUME would satisfy a container probe. Model
      # that cross-match ONLY for the generic form so the typed-form case below
      # fails if the probe is reverted.
      [[ "${2:-}" == inspect ]] || return 0
      [[ -e "$FIXTURE_ROOT/root/containers/$3" ]] && return 0
      [[ -e "$FIXTURE_ROOT/root/volumes/$3"* ]] 2>/dev/null
      return $? ;;
    *) return 0 ;;
  esac
}
STORE

TENANT=alice
PG_CONTAINER="lunarwing-pg-$TENANT"
PG_VOLUME="lunarwing-pg-$TENANT"

# ── probe unit tests (in-process) ─────────────────────────────────────────────
export FIXTURE_ROOT="$fixture"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"
# shellcheck source=/dev/null
source "$fixture/store.sh"

# empty store → absent
if _rootless_pg_data_present "$TENANT" "$PG_CONTAINER"; then
  fail "empty store counted as rootless data present"
else
  pass "no container and no volume: data absent"
fi

# container present → present
touch "$fixture/rootless/containers/$PG_CONTAINER"
if _rootless_pg_data_present "$TENANT" "$PG_CONTAINER"; then
  pass "container present: data present"
else
  fail "container arm of _rootless_pg_data_present broken"
fi
rm -f "$fixture/rootless/containers/$PG_CONTAINER"

# exact-name volume → present (Quadlet --rm restart case)
touch "$fixture/rootless/volumes/$PG_VOLUME"
if _rootless_pg_data_present "$TENANT" "$PG_CONTAINER"; then
  pass "exact-name volume: data present (Quadlet --rm restart case)"
else
  fail "exact-name volume arm broken — legitimate migrated restart would die"
fi

# prefix volume (lunarwing-pg-<t>2 exists, lunarwing-pg-<t> does not) → absent:
# volume inspect returns lunarwing-pg-<t>2 and the guard must compare for
# equality. A mutant that tests merely "non-empty" fails here.
rm -f "$fixture/rootless/volumes/$PG_VOLUME"
touch "$fixture/rootless/volumes/${PG_VOLUME}2"
if _rootless_pg_data_present "$TENANT" "$PG_CONTAINER"; then
  fail "prefix volume ${PG_VOLUME}2 wrongly counted as ${PG_VOLUME} data present"
else
  pass "prefix volume does not satisfy the equality probe"
fi
rm -f "$fixture/rootless/volumes/${PG_VOLUME}2"

# root-store volume must NOT satisfy the typed container probe (RZ-2)
touch "$fixture/root/volumes/${PG_VOLUME}-rollback"
if _root_store_pg_container_present "$TENANT" "$PG_CONTAINER"; then
  fail "root-store VOLUME satisfied the root-container probe (typed form regressed)"
else
  pass "root-store volume does not satisfy the typed container probe"
fi
rm -f "$fixture/root/volumes/${PG_VOLUME}-rollback"

# root-store container present → probe true
touch "$fixture/root/containers/$PG_CONTAINER"
if _root_store_pg_container_present "$TENANT" "$PG_CONTAINER"; then
  pass "root container present: probe true"
else
  fail "root-store container arm broken"
fi

# ── the guard decision through start_tenant_postgres, in a child bash ─────────
# die() exits its shell by design, so each decision case runs as a child
# process with the fixture store + the openrc imperative branch pinned.
run_start_pg() {  # -> sets pg_rc and pg_output
  if FIXTURE_ROOT="$fixture" bash -c '
    set -uo pipefail
    source "$1"
    source "$2"
    INIT_SYSTEM=openrc; CONTAINER_RT=podman; MT_ROOTLESS=true
    ensure_init_system() { :; }
    podman_supports_quadlet() { return 1; }
    _wait_user_manager() { :; }
    _openrc_podman_uses_init_health() { return 1; }
    ports_get() { printf "10013\n"; }
    tenant_pg_password() { printf "fixture-pg-password\n"; }
    write_tenant_file_atomic() { return 0; }
    start_tenant_postgres "$3"
  ' _ "$MT_ADMIN" "$fixture/store.sh" "$TENANT" >"$fixture/pg.out" 2>&1
  then pg_rc=0; else pg_rc=$?; fi
  pg_output="$(command cat "$fixture/pg.out")"
}

# Case 1 — the guard window: rootless data absent + root container present → die
rm -f "$fixture/rootless/containers/$PG_CONTAINER" "$fixture/rootless/volumes/$PG_VOLUME"
run_start_pg
if [[ "$pg_rc" -eq 1 && "$pg_output" == *"refusing to create an empty rootless PG over existing root-store data"* ]]; then
  pass "guard fires in the rootless-absent + root-container window"
else
  fail "guard did not die in its window (rc=$pg_rc): $pg_output"
fi

# Case 2 — ack env silences the guard and the start proceeds
run_start_pg_with_ack() {
  if FIXTURE_ROOT="$fixture" LUNARWING_MT_ACK_ROOTLESS_FLIP="$TENANT" bash -c '
    set -uo pipefail
    source "$1"
    source "$2"
    INIT_SYSTEM=openrc; CONTAINER_RT=podman; MT_ROOTLESS=true
    ensure_init_system() { :; }
    podman_supports_quadlet() { return 1; }
    _wait_user_manager() { :; }
    _openrc_podman_uses_init_health() { return 1; }
    ports_get() { printf "10013\n"; }
    tenant_pg_password() { printf "fixture-pg-password\n"; }
    write_tenant_file_atomic() { return 0; }
    start_tenant_postgres "$3"
  ' _ "$MT_ADMIN" "$fixture/store.sh" "$TENANT" >"$fixture/pg.out" 2>&1
  then pg_rc=0; else pg_rc=$?; fi
  pg_output="$(command cat "$fixture/pg.out")"
}
run_start_pg_with_ack
if [[ "$pg_rc" -eq 0 ]]; then
  pass "ack env silences the guard"
else
  fail "ack env path failed (rc=$pg_rc): $pg_output"
fi

# Case 3 — rootless volume present (migrated tenant, Quadlet --rm stopped the
# container): no guard fire, normal start
rm -f "$fixture/root/containers/$PG_CONTAINER"
touch "$fixture/rootless/volumes/$PG_VOLUME"
run_start_pg
if [[ "$pg_rc" -eq 0 && "$pg_output" != *"DATA-ORPHAN GUARD"* ]]; then
  pass "exact-name rootless volume prevents the guard (RW-2 fix holds)"
else
  fail "guard fired on a migrated tenant with kept volume (rc=$pg_rc): $pg_output"
fi
rm -f "$fixture/rootless/volumes/$PG_VOLUME"

# Case 4 — fresh tenant (no root container): guard silent, start proceeds
run_start_pg
if [[ "$pg_rc" -eq 0 && "$pg_output" != *"DATA-ORPHAN GUARD"* ]]; then
  pass "fresh tenant never trips the guard"
else
  fail "guard fired on a fresh tenant (rc=$pg_rc): $pg_output"
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %s failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: PG data-orphan guard pins passed (RZ-3)\n'
