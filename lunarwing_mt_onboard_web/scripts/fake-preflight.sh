#!/usr/bin/env bash
# Demo stand-in for upgrade-preflight.sh.
set -uo pipefail
SLEEP="${LUNARWING_DEMO_SLEEP:-0.3}"
nap() { sleep "$SLEEP"; }
tenant="${1:-demo}"

echo "=== Upgrade preflight: $tenant ==="
echo "--- Detecting source version ---"; nap
echo "detected source version v1.1.7"; nap
echo "--- Checking filesystem headroom ---"; nap
echo "free space OK (48G available)"; nap
echo "--- Checking service health ---"; nap
if [[ "$tenant" == *fail* ]]; then
  echo "preflight: tenant is unhealthy, refusing to continue" >&2
  echo "=== Preflight FAILED ===" >&2
  exit 1
fi
echo "all preflight checks passed"; nap
echo "=== Preflight OK for '$tenant' ==="
