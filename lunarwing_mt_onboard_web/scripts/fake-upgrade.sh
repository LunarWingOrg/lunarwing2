#!/usr/bin/env bash
# Demo stand-in for upgrade-tenant-version.sh.
set -uo pipefail
SLEEP="${LUNARWING_DEMO_SLEEP:-0.3}"
nap() { sleep "$SLEEP"; }

tenant="${1:-demo}"
shift || true
apply="no"
target="(auto)"
prev=""
for a in "$@"; do
  case "$a" in
    --apply) apply="yes" ;;
    --target) prev="target" ; continue ;;
  esac
  if [ "$prev" = "target" ]; then target="$a"; prev=""; fi
done

mode="DRY-RUN"; [ "$apply" = "yes" ] && mode="APPLY"
echo "=== Upgrading tenant '$tenant' to ${target} [${mode}] ==="
echo "--- Backing up database ---"; nap
echo "pg_dump complete (backup staged)"; nap
echo "--- Fetching target release ---"; nap
for p in 20 55 90 100; do echo "Receiving objects: ${p}%"; nap; done
echo "--- Rebuilding binaries ---"
for c in lunarwing-core gateway; do echo "   Compiling $c"; nap; done
echo "--- Running schema migrations ---"; nap
if [ "$apply" = "yes" ]; then
  echo "applied 3 migrations"; nap
  echo "restarted lunarwing-$tenant"; nap
  echo "=== Upgrade APPLIED for '$tenant' ==="
else
  echo "[dry-run] would apply 3 migrations"; nap
  echo "[dry-run] would restart lunarwing-$tenant"; nap
  echo "=== Dry-run complete for '$tenant' (no changes made) ==="
fi
