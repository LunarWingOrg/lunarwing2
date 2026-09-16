#!/usr/bin/env bash
# Migrate the multi-tenant port registry from v16 to v17.
# Adds a dedicated two-port range (51000-51999) for the OpenCode 2 external
# worker: per-tenant opencode2_ports {opencode2_wss, opencode2_health}. The
# v2 worker is opt-in per tenant; .workers.opencode2 stays false until
# selected, so this migration changes no live ports.

set -euo pipefail

PORTS_REGISTRY="${PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
# Optional override so operators can pin a different range start.
OPENCODE2_RANGE_START="${OPENCODE2_RANGE_START:-51000}"

if [[ "$PORTS_REGISTRY" == "/etc/lunarwing/ports.json" && $EUID -ne 0 ]]; then
  echo "error: must run as root" >&2
  exit 1
fi
[[ -f "$PORTS_REGISTRY" ]] || { echo "error: $PORTS_REGISTRY not found" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "error: jq is required" >&2; exit 1; }
command -v flock >/dev/null 2>&1 || { echo "error: flock is required" >&2; exit 1; }

exec 199<"$(dirname "$PORTS_REGISTRY")"
flock -x 199
current_version="$(jq -r '.version // 0' "$PORTS_REGISTRY")"
if [[ "$current_version" -ge 17 ]]; then
  echo "already at v${current_version}, nothing to do"
  exit 0
fi
if [[ "$current_version" -lt 16 ]]; then
  echo "error: registry at v${current_version}; migrate through v16 first" >&2
  exit 1
fi
[[ "$OPENCODE2_RANGE_START" =~ ^[0-9]+$ ]] || { echo "error: OPENCODE2_RANGE_START must be numeric" >&2; exit 1; }

backup="${PORTS_REGISTRY}.bak.$(date -u +%Y%m%d%H%M%S)"
cp -p "$PORTS_REGISTRY" "$backup"
echo "backed up registry to $backup"

range_end=$((OPENCODE2_RANGE_START + 999))
tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
jq --argjson ostart "$OPENCODE2_RANGE_START" --argjson oend "$range_end" '
    .version = 17
    | .opencode2_range = ((.opencode2_range) // {"start": $ostart, "end": $oend})
    | .opencode2_block_size = ((.opencode2_block_size) // 2)
    | ( .range.start // 10000 ) as $rstart
    | ( .opencode2_range.start ) as $ostart
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $ostart ) as $obase
            | .opencode2_base = ((.opencode2_base) // $obase)
            | .opencode2_ports = ((.opencode2_ports) // {
                opencode2_wss: ($obase + 0),
                opencode2_health: ($obase + 1)
              })
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"

collisions="$(jq -r '
  [ .tenants[]
    | ((.ports // {}) | to_entries[] | .value),
      ((.extended_ports // {}) | to_entries[] | .value),
      ((.kimi_ports // {}) | to_entries[] | .value),
      ((.voice_ports // {}) | to_entries[] | .value),
      ((.opencode2_ports // {}) | to_entries[] | .value)
  ]
  | group_by(.) | map(select(length > 1)) | length
' "$tmp")"
health_metrics_conflict="$(jq -r --argjson ostart "$OPENCODE2_RANGE_START" '
  ((.health_metrics_port // 0) >= $ostart and (.health_metrics_port // 0) <= ($ostart + 999)) | tostring
' "$tmp")"
if [[ "$collisions" -ne 0 ]]; then
  echo "error: migration would create $collisions colliding port(s); registry unchanged" >&2
  echo "       inspect candidate: $tmp" >&2
  exit 1
fi
if [[ "$health_metrics_conflict" == "true" ]]; then
  echo "error: opencode2 range overlaps the health_metrics slot; set OPENCODE2_RANGE_START and retry" >&2
  rm -f "$tmp"
  exit 1
fi

chmod 0644 "$tmp"
mv "$tmp" "$PORTS_REGISTRY"
echo "port registry migrated to v17 (opencode2 range ${OPENCODE2_RANGE_START}-${range_end})"
echo "rollback if needed: cp $backup $PORTS_REGISTRY"
