#!/usr/bin/env bash
# kageho-wide-purge-20260915.sh — POST-CORRUPTION WIDE PURGE (v1)
# Purpose: remove self-carried corrupted context after surface clear.
# Blasts: (1) ALL runtime threads/events/leases/conversations,
#         (2) dailies Sep 12-15 (corruption era, whole files not token-filtered),
#         (3) MEMORY.md-class memory docs updated during the era,
#         (4) leftover mission goal-artifacts (echo state).
# Safety: pg_dump SNAPSHOT FIRST (auto, aborts if dump fails).
#         Verify-then-execute: prints full target list, requires typed PURGE.
#         Daemon MUST be stopped (gate below refuses otherwise).
# Usage: sudo bash kageho-wide-purge-20260915.sh kageho
set -euo pipefail
TENANT="${1:?usage: kageho-wide-purge-20260915.sh <tenant>}"

PGPORT=$(python3 -c "import json;print(json.load(open('/etc/lunarwing/ports.json'))['tenants']['$TENANT']['ports']['postgres'])" 2>/dev/null)
ENVF="/home/$TENANT/lunarwing/env/lunarwing.env"
DBURL=$(grep -A1 '^DATABASE_URL=' "$ENVF" | tr '\n' ' ' | sed 's/\\ / /' | sed -E 's#.*DATABASE_URL="?([^" ]+)"?.*#\1#')
export PGPASSWORD=$(echo "$DBURL" | sed -E 's#.*://[^:]+:([^@]+)@.*#\1#')
PGUSER=$(echo "$DBURL" | sed -E 's#.*://([^:]+):.*#\1#')
PGDB=$(echo "$DBURL" | sed -E 's#.*/([^/?]+)(\?.*)?#\1#')

if [ -n "${PSQL_CMD:-}" ]; then
  PSQL="$PSQL_CMD"
elif command -v psql >/dev/null 2>&1; then
  PSQL="psql -h 127.0.0.1 -p $PGPORT -U $PGUSER -d $PGDB -At -q"
else
  FOUND=""
  for CAND in $(podman ps --format '{{.Names}}' 2>/dev/null | grep -iE 'pg|postgres' | grep -i "$TENANT"); do
    if podman exec "$CAND" psql -U "$PGUSER" -d "$PGDB" -At -q -c 'SELECT 1' >/dev/null 2>&1; then
      FOUND="podman exec -i $CAND psql -U $PGUSER -d $PGDB -At -q"; break
    fi
  done
  [ -n "$FOUND" ] || { echo "ERROR: no psql path for tenant $TENANT" >&2; exit 3; }
  PSQL="$FOUND"
fi

# ---- Gate 1: daemon must be STOPPED (re-persistence eats live deletes) ----
if rc-service lunarwing-$TENANT status >/dev/null 2>&1; then
  if rc-service lunarwing-$TENANT status 2>&1 | grep -qE '^\s*status:\s*started'; then
    echo "REFUSING: rc-service lunarwing-$TENANT is STARTED. Stop it first:" >&2
    echo "  rc-service lunarwing-$TENANT stop" >&2
    exit 2
  fi
fi
STILL=$(pgrep -u "$TENANT" -af lunarwing 2>/dev/null | grep -vE 'conmon|podman|postgres|crun|pg-|xmpp-bridge|health-metrics|ocr-sidecar|weechat|tmux' || true)
if [ -n "$STILL" ]; then
  echo "REFUSING: tenant daemon process(es) still alive:" >&2
  echo "$STILL" >&2
  exit 2
fi

# ---- Gate 2: snapshot FIRST ----
DUMP=/tmp/${TENANT}-wide-purge-snapshot-$(date -u +%Y%m%dT%H%M%SZ).sql
echo "== snapshotting to $DUMP =="
pg_dump -h 127.0.0.1 -p "$PGPORT" -U "$PGUSER" "$PGDB" > "$DUMP"
[ -s "$DUMP" ] || { echo "ABORT: dump is empty" >&2; exit 2; }
echo "snapshot OK: $(du -h "$DUMP" | cut -f1)"

# ---- Targets ----
echo
echo "== TARGET PREVIEW =="
echo "-- [T1] ALL engine/.runtime docs (threads/events/leases/checkpoints):"
$PSQL -c "SELECT count(*), pg_size_pretty(sum(length(content))) FROM memory_documents WHERE path LIKE 'engine/.runtime/%';"
echo "-- [T2] conversation surfaces (all channels):"
$PSQL -c "SELECT count(*) FROM conversations;" 2>/dev/null || \
  $PSQL -c "SELECT count(*) FROM memory_documents WHERE path LIKE 'engine/conversations%' OR path LIKE 'engine/.runtime/conversations%';"
echo "-- [T3] dailies Sep 12-15 (whole files):"
$PSQL -c "SELECT path, to_char(updated_at,'MM-DD HH24:MI') AS at, length(content) AS len FROM memory_documents WHERE path LIKE 'daily/%' AND updated_at >= '2026-09-12 00:00:00+00' ORDER BY path;"
echo "-- [T4] memory/knowledge docs UPDATED during corruption era (Sep 14-15):"
$PSQL -c "SELECT path, to_char(updated_at,'MM-DD HH24:MI') AS at, length(content) AS len FROM memory_documents WHERE (path LIKE 'engine/knowledge%' OR path LIKE 'engine/skills%' OR path LIKE 'engine/study%' OR path LIKE 'MEMORY%' OR path LIKE 'memory/%') AND updated_at >= '2026-09-14 00:00:00+00' AND path NOT LIKE 'engine/projects/%' ORDER BY updated_at;"
echo "-- [T5] mission goal-artifacts + priority orphans under missions:"
$PSQL -c "SELECT path FROM memory_documents WHERE path LIKE 'engine/projects/%/missions/%' AND path NOT LIKE '%/mission.json';"

echo
echo "== REVIEW THE LISTS ABOVE =="
echo "T1+T2 are whole-class deletes (runtime state + surfaces — intentional)."
echo "T3 deletes listed daily files entirely. T4 is knowledge-layer — REVIEW each row."
echo "T5 is mission output artifacts."
echo
read -r -p "Type PURGE to execute, anything else aborts: " CONFIRM
[ "$CONFIRM" = "PURGE" ] || { echo "aborted, nothing deleted"; exit 0; }

$PSQL <<'SQL'
BEGIN;
-- T1: all runtime state
DELETE FROM memory_documents WHERE path LIKE 'engine/.runtime/%';
-- T2: conversation surfaces (either storage shape)
DELETE FROM conversations;
DELETE FROM memory_documents WHERE path LIKE 'engine/conversations%' OR path LIKE 'engine/.runtime/conversations%';
-- T3: corruption-era dailies, whole files
DELETE FROM memory_documents WHERE path LIKE 'daily/%' AND updated_at >= '2026-09-12 00:00:00+00';
-- T4: knowledge-layer docs touched during the corruption era
DELETE FROM memory_documents WHERE (path LIKE 'engine/knowledge%' OR path LIKE 'engine/skills%' OR path LIKE 'engine/study%' OR path LIKE 'MEMORY%' OR path LIKE 'memory/%') AND updated_at >= '2026-09-14 00:00:00+00' AND path NOT LIKE 'engine/projects/%';
-- T5: mission non-mission.json artifacts
DELETE FROM memory_documents WHERE path LIKE 'engine/projects/%/missions/%' AND path NOT LIKE '%/mission.json';
COMMIT;
SQL

echo
echo "== POST-CHECKS =="
echo "-- residual runtime docs (expect 0):"
$PSQL -c "SELECT count(*) FROM memory_documents WHERE path LIKE 'engine/.runtime/%';"
echo "-- residual conversations (expect 0):"
$PSQL -c "SELECT count(*) FROM conversations;" 2>/dev/null || echo "(table absent — fine)"
echo "-- residual corruption-era dailies (expect 0):"
$PSQL -c "SELECT count(*) FROM memory_documents WHERE path LIKE 'daily/%' AND updated_at >= '2026-09-12 00:00:00+00';"
echo "-- vocab recount whole DB (expect 0):"
$PSQL -c "SELECT count(*) FROM memory_documents WHERE content ILIKE '%moopig%' OR content ILIKE '%lumacoin%' OR content ILIKE '%zoi-notifier%';"
echo "-- surviving docs + missions sanity:"
$PSQL -c "SELECT count(*) AS docs FROM memory_documents;"
$PSQL -c "SELECT path FROM memory_documents WHERE path LIKE 'engine/projects/%/missions/%/mission.json';"
echo
echo "DONE. Snapshot at $DUMP — keep it (forensic evidence)."
echo "Start her with: rc-service lunarwing-$TENANT start"
echo "First session: she wakes with empty history. Expect disorientation; her WI#110 sentinel"
echo "and mission set survive. Do NOT let her re-read the incident from the snapshot."
