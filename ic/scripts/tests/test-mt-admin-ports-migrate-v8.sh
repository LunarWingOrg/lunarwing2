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

echo
if ((failures == 0)); then
  echo "ALL PASS"
else
  echo "$failures FAILURE(S)"
  exit 1
fi
