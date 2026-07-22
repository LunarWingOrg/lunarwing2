#!/usr/bin/env bash
# Demo stand-in for import-tenant.sh.
set -uo pipefail
SLEEP="${LUNARWING_DEMO_SLEEP:-0.3}"
nap() { sleep "$SLEEP"; }

bundle="${1:-demo.7z}"
shift || true
apply="yes"
start="no"
old_stopped="no"
name=""
while (($#)); do
  case "$1" in
    --dry-run) apply="no"; shift ;;
    --start) start="yes"; shift ;;
    --old-stopped) old_stopped="yes"; shift ;;
    --name) name="${2:-}"; shift 2 ;;
    --owner-scope) shift 2 ;;
    *) shift ;;
  esac
done

target="${name:-restored-tenant}"
mode="APPLY"; [ "$apply" = "no" ] && mode="DRY-RUN"
echo "=== Importing ${bundle} as '${target}' [${mode}] ==="
echo "--- Provision target tenant ---"; nap
echo "--- Build requested workers and extensions ---"; nap
echo "--- Inject restored manifests ---"; nap
echo "--- Restore database and state ---"; nap
echo "--- Stage restored tenant ---"; nap

if [ "$apply" = "no" ]; then
  echo "[dry-run] would stage '${target}' without changing the host"; nap
fi

if [ "$start" = "yes" ]; then
  if [ "$old_stopped" != "yes" ]; then
    echo "refusing unattended start: old host must be stopped (--old-stopped)" >&2
    exit 2
  fi
  echo "--- Start restored tenant ---"; nap
  echo "=== Import complete; '${target}' started ==="
else
  echo "=== Import complete; '${target}' remains staged ==="
fi
