#!/usr/bin/env bash
# Regression tests for work item #379: the inline v7 -> v8 port registry
# migration must mirror the standalone scripts/migrate-ports-v8.sh safety net.
#
# Covers:
#   - happy path: reserved_3/reserved_4 are renamed to nanocode_health /
#     pebble_health, the version bumps to 8, and a timestamped `.bak.*` copy of
#     the pre-migration registry is left behind with a rollback hint.
#   - collision refusal: a candidate that leaves one numeric port assigned to
#     more than one slot aborts before the swap — non-zero exit, the live
#     registry byte-identical to its pre-run content, the backup written, and
#     no candidate `.tmp.*` left in the registry directory.
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
assert_file_eq() {  # <label> <file-a> <file-b>
  if cmp -s "$2" "$3"; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        differs: [$2] vs [$3]"
    failures=$((failures + 1))
  fi
}
assert_no_glob() {  # <label> <pattern>
  local matches
  matches="$(compgen -G "$2" || true)"
  assert_eq "$1" "$matches" ""
}

FIXTURE="$(mktemp -d)"
cleanup() { rm -rf "$FIXTURE"; }
trap cleanup EXIT

cat >"$FIXTURE/clean-v7.json" <<'JSON'
{
  "version": 7,
  "range": {"start": 10000, "end": 19999},
  "extended_range": {"start": 20000, "end": 29999},
  "block_size": 10,
  "extended_block_size": 10,
  "tenants": {
    "alpha": {
      "base_port": 10000,
      "ports": {
        "gateway": 10000, "http": 10001, "bridge": 10002,
        "postgres": 10003, "proxy": 10004, "weechat": 10005,
        "orchestrator": 10006, "nanocode_wss": 10007,
        "pebble_wss": 10008, "weechat_adapter": 10009
      },
      "extended_base": 20000,
      "extended_ports": {
        "darkirc_adapter": 20000, "darkirc_irc": 20001, "darkirc_rpc": 20002,
        "reserved_3": 20003, "reserved_4": 20004, "reserved_5": 20005,
        "reserved_6": 20006, "reserved_7": 20007, "reserved_8": 20008,
        "reserved_9": 20009
      }
    },
    "beta": {
      "base_port": 10010,
      "ports": {
        "gateway": 10010, "http": 10011, "bridge": 10012,
        "postgres": 10013, "proxy": 10014, "weechat": 10015,
        "orchestrator": 10016, "nanocode_wss": 10017,
        "pebble_wss": 10018, "weechat_adapter": 10019
      },
      "extended_base": 20010,
      "extended_ports": {
        "darkirc_adapter": 20010, "darkirc_irc": 20011, "darkirc_rpc": 20012,
        "reserved_3": 20013, "reserved_4": 20014, "reserved_5": 20015
      }
    }
  }
}
JSON

echo "=== ports_migrate_v8 happy path ==="

PORTS_REGISTRY="$FIXTURE/happy.json"
cp "$FIXTURE/clean-v7.json" "$PORTS_REGISTRY"
current_version=7
migration_out="$(ports_migrate_v8)"

assert_eq "version bumped to 8" \
  "$(jq -r '.version' "$PORTS_REGISTRY")" "8"
assert_eq "alpha nanocode_health dedicated from reserved_3" \
  "$(jq -r '.tenants.alpha.extended_ports.nanocode_health' "$PORTS_REGISTRY")" "20003"
assert_eq "alpha pebble_health dedicated from reserved_4" \
  "$(jq -r '.tenants.alpha.extended_ports.pebble_health' "$PORTS_REGISTRY")" "20004"
assert_eq "beta nanocode_health falls back to extended_base+3" \
  "$(jq -r '.tenants.beta.extended_ports.nanocode_health' "$PORTS_REGISTRY")" "20013"
assert_eq "beta pebble_health dedicated from reserved_4" \
  "$(jq -r '.tenants.beta.extended_ports.pebble_health' "$PORTS_REGISTRY")" "20014"
assert_eq "reserved_3/4 consumed for both tenants" \
  "$(jq -r '[.tenants[].extended_ports | has("reserved_3") or has("reserved_4")] | any' "$PORTS_REGISTRY")" "false"
assert_eq "migrated registry reports no duplicate ports" \
  "$(ports_registry_duplicate_ports)" ""

backups=()
while IFS= read -r path; do
  [[ -n "$path" ]] && backups+=("$path")
done < <(compgen -G "$PORTS_REGISTRY.bak.*" || true)
assert_eq "exactly one backup written" "${#backups[@]}" "1"
if ((${#backups[@]} == 1)); then
  assert_file_eq "backup holds the pre-migration registry" \
    "${backups[0]}" "$FIXTURE/clean-v7.json"
fi
assert_contains "output names the backup" "$migration_out" "backed up port registry to"
assert_contains "output prints the rollback command" "$migration_out" "rollback if needed: cp"
assert_no_glob "no candidate temp file left behind" "$PORTS_REGISTRY.tmp.*"

echo "=== ports_migrate_v8 collision refusal ==="

# Pre-existing cross-tenant duplicate: beta.gateway steals alpha.gateway's
# port. The rename itself is collision-neutral, but the candidate must be
# checked before it replaces the live registry.
jq '.tenants.beta.ports.gateway = 10000' \
  "$FIXTURE/clean-v7.json" >"$FIXTURE/collide-v7.json"
PORTS_REGISTRY="$FIXTURE/collide.json"
cp "$FIXTURE/collide-v7.json" "$PORTS_REGISTRY"
# shellcheck disable=SC2034  # read by ports_migrate_v8 (sourced library)
current_version=7

rc=0
refusal_out="$( ( ports_migrate_v8 ) 2>&1 )" || rc=$?

assert_eq "collision refusal exits non-zero" "$rc" "1"
assert_contains "refusal names the duplicate port" "$refusal_out" "10000"
assert_contains "refusal states the registry is unchanged" "$refusal_out" "registry unchanged"
assert_file_eq "live registry left byte-identical" \
  "$PORTS_REGISTRY" "$FIXTURE/collide-v7.json"
assert_eq "version stays at 7 after refusal" \
  "$(jq -r '.version' "$PORTS_REGISTRY")" "7"

backups=()
while IFS= read -r path; do
  [[ -n "$path" ]] && backups+=("$path")
done < <(compgen -G "$PORTS_REGISTRY.bak.*" || true)
assert_eq "refusal still leaves one backup" "${#backups[@]}" "1"
if ((${#backups[@]} == 1)); then
  assert_file_eq "refusal backup holds the unchanged registry" \
    "${backups[0]}" "$FIXTURE/collide-v7.json"
fi
assert_no_glob "refusal removes the candidate temp file" "$PORTS_REGISTRY.tmp.*"

echo "=== ports_migrate_v8 unparseable registry refusal (NY-9) ==="

# A corrupt registry (tenant b's .ports is a string) passes the rename jq
# (which only touches .extended_ports) but the post-rename collision query
# fails with 'string ("corrupt") has no keys'. Before NY-9 the inline guard
# swallowed that jq error and swapped the candidate; the standalone script
# refused it. The guard must now fail closed: the candidate is removed, the
# live registry stays byte-identical, and the refusal names the backup.
cat >"$FIXTURE/corrupt-v7.json" <<'JSON'
{
  "version": 7,
  "range": {"start": 10000, "end": 19999},
  "extended_range": {"start": 20000, "end": 29999},
  "block_size": 10,
  "extended_block_size": 10,
  "tenants": {
    "alpha": {
      "base_port": 10000,
      "ports": {"gateway": 10000, "http": 10001},
      "extended_base": 20000,
      "extended_ports": {"reserved_3": 20003, "reserved_4": 20004}
    },
    "beta": {
      "base_port": 10010,
      "ports": "corrupt",
      "extended_base": 20010,
      "extended_ports": {"reserved_3": 10000, "reserved_4": 20014}
    }
  }
}
JSON
PORTS_REGISTRY="$FIXTURE/corrupt.json"
cp "$FIXTURE/corrupt-v7.json" "$PORTS_REGISTRY"
# shellcheck disable=SC2034  # read by ports_migrate_v8 (sourced library)
current_version=7

rc=0
unparseable_out="$( ( ports_migrate_v8 ) 2>&1 )" || rc=$?

assert_eq "unparseable registry refusal exits non-zero" "$rc" "1"
assert_contains "refusal says it could not verify uniqueness" \
  "$unparseable_out" "could not verify port uniqueness"
assert_contains "refusal states the registry is unchanged" \
  "$unparseable_out" "registry unchanged"
assert_file_eq "live registry left byte-identical" \
  "$PORTS_REGISTRY" "$FIXTURE/corrupt-v7.json"
assert_eq "version stays at 7 after unparseable refusal" \
  "$(jq -r '.version' "$PORTS_REGISTRY")" "7"

backups=()
while IFS= read -r path; do
  [[ -n "$path" ]] && backups+=("$path")
done < <(compgen -G "$PORTS_REGISTRY.bak.*" || true)
assert_eq "unparseable refusal still leaves one backup" "${#backups[@]}" "1"
if ((${#backups[@]} == 1)); then
  assert_file_eq "unparseable refusal backup holds the unchanged registry" \
    "${backups[0]}" "$FIXTURE/corrupt-v7.json"
fi
assert_no_glob "unparseable refusal removes the candidate temp file" \
  "$PORTS_REGISTRY.tmp.*"

echo "=== ports_migrate_v8 rename-jq failure cleanup (NY-11) ==="

# A registry whose extended_base is a string makes the rename jq itself fail
# (string + number). The candidate temp file is written empty; the migration
# must remove it and die naming the backup. Before NY-11 this cleanup was
# untested.
cat >"$FIXTURE/rename-fail-v7.json" <<'JSON'
{
  "version": 7,
  "range": {"start": 10000, "end": 19999},
  "block_size": 10,
  "tenants": {
    "alpha": {
      "base_port": 10000,
      "ports": {"gateway": 10000, "http": 10001},
      "extended_base": "oops",
      "extended_ports": {"reserved_3": 20003}
    }
  }
}
JSON
PORTS_REGISTRY="$FIXTURE/rename-fail.json"
cp "$FIXTURE/rename-fail-v7.json" "$PORTS_REGISTRY"
# shellcheck disable=SC2034  # read by ports_migrate_v8 (sourced library)
current_version=7

rc=0
rename_fail_out="$( ( ports_migrate_v8 ) 2>&1 )" || rc=$?

assert_eq "rename-jq failure exits non-zero" "$rc" "1"
assert_contains "rename-jq failure states the registry is unchanged" \
  "$rename_fail_out" "registry unchanged"
assert_file_eq "rename-jq failure leaves the live registry byte-identical" \
  "$PORTS_REGISTRY" "$FIXTURE/rename-fail-v7.json"
assert_eq "version stays at 7 after rename-jq failure" \
  "$(jq -r '.version' "$PORTS_REGISTRY")" "7"
assert_no_glob "rename-jq failure removes the candidate temp file" \
  "$PORTS_REGISTRY.tmp.*"

echo "=== ports_migrate_v8 fresh-registry skip-backup (NY-10) ==="

# A freshly-seeded registry (ports_registry_init created it this invocation)
# has no prior live state to roll back to. ports_migrate_v8 must not write a
# .bak.* or print a rollback hint for it. The init path sets
# ports_migrate_registry_preexisted=0 before ports_migrate runs; simulating
# that here mirrors the fresh-host ladder.
PORTS_REGISTRY="$FIXTURE/fresh.json"
cp "$FIXTURE/clean-v7.json" "$PORTS_REGISTRY"
# shellcheck disable=SC2034  # read by ports_migrate_v8 (sourced library)
current_version=7
# shellcheck disable=SC2034  # read by ports_migrate_v8 (sourced library)
ports_migrate_registry_preexisted=0

fresh_out="$(ports_migrate_v8)"

assert_eq "fresh registry still bumps to v8" \
  "$(jq -r '.version' "$PORTS_REGISTRY")" "8"
assert_eq "fresh registry reports no duplicate ports" \
  "$(ports_registry_duplicate_ports)" ""
assert_no_glob "fresh registry writes no backup" "$PORTS_REGISTRY.bak.*"
assert_no_glob "fresh registry leaves no candidate temp" "$PORTS_REGISTRY.tmp.*"
case "$fresh_out" in
  *"rollback if needed"*)
    echo "  FAIL: fresh registry printed a rollback hint"
    echo "        in: [$fresh_out]"
    failures=$((failures + 1))
    ;;
  *) echo "  PASS: fresh registry prints no rollback hint" ;;
esac
case "$fresh_out" in
  *"backed up port registry"*)
    echo "  FAIL: fresh registry announced a backup"
    echo "        in: [$fresh_out]"
    failures=$((failures + 1))
    ;;
  *) echo "  PASS: fresh registry announces no backup" ;;
esac

echo
if ((failures == 0)); then
  echo "ALL PASS"
else
  echo "$failures FAILURE(S)"
  exit 1
fi
