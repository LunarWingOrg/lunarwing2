#!/usr/bin/env bash
# Demo stand-in for export-tenant.sh.
set -uo pipefail
SLEEP="${LUNARWING_DEMO_SLEEP:-0.3}"
nap() { sleep "$SLEEP"; }

tenant="${1:-demo}"
shift || true
apply="yes"
out_dir="/var/lib/lunarwing-migrate"
prev=""
for a in "$@"; do
  case "$a" in
    --dry-run) apply="no" ;;
    --out-dir) prev="out" ; continue ;;
  esac
  if [ "$prev" = "out" ]; then out_dir="$a"; prev=""; fi
done

mode="APPLY"; [ "$apply" = "no" ] && mode="DRY-RUN"
echo "=== Exporting tenant '$tenant' -> ${out_dir} [${mode}] ==="
echo "--- Quiescing services ---"; nap
echo "--- Dumping database ---"; nap
for p in 25 60 100; do echo "pg_dump: ${p}%"; nap; done
echo "--- Bundling home tree ---"; nap
for p in 30 70 100; do echo "tar: ${p}%"; nap; done
if [ "$apply" = "no" ]; then
  echo "[dry-run] would write ${out_dir}/${tenant}-migrate-demo.7z"; nap
  echo "=== Dry-run complete (no bundle written) ==="
else
  echo "wrote ${out_dir}/${tenant}-migrate-demo.7z (1.2G, AES-256 encrypted)"; nap
  echo "=== Export complete for '$tenant' ==="
fi
