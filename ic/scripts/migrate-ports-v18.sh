#!/usr/bin/env bash
# Migrate the multi-tenant port registry from v17 to v18, adding opt-in LunarDepth ports.

set -euo pipefail

PORTS_REGISTRY="${PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
LUNARDEPTH_RANGE_START="${LUNARDEPTH_RANGE_START:-52000}"

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
if [[ "$current_version" -ge 18 ]]; then
  echo "already at v${current_version}, nothing to do"
  exit 0
fi
if [[ "$current_version" -lt 17 ]]; then
  echo "error: registry at v${current_version}; migrate through v17 first" >&2
  exit 1
fi
[[ "$LUNARDEPTH_RANGE_START" =~ ^[0-9]+$ ]] || { echo "error: LUNARDEPTH_RANGE_START must be numeric" >&2; exit 1; }

backup="${PORTS_REGISTRY}.bak.$(date -u +%Y%m%d%H%M%S)"
cp -p "$PORTS_REGISTRY" "$backup"
echo "backed up registry to $backup"

range_end=$((LUNARDEPTH_RANGE_START + 999))
tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
jq --argjson lstart "$LUNARDEPTH_RANGE_START" --argjson lend "$range_end" '
    .version = 18
    | .lunardepth_range = ((.lunardepth_range) // {"start": $lstart, "end": $lend})
    | .lunardepth_block_size = ((.lunardepth_block_size) // 2)
    | ( .range.start // 10000 ) as $rstart
    | ( .lunardepth_range.start ) as $lstart
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $lstart ) as $lbase
            | .lunardepth_base = ((.lunardepth_base) // $lbase)
            | .lunardepth_ports = ((.lunardepth_ports) // {
                lunardepth_wss: ($lbase + 0),
                lunardepth_health: ($lbase + 1)
              })
            | .workers.lunardepth = ((.workers.lunardepth) // false)
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
      ((.opencode2_ports // {}) | to_entries[] | .value),
      ((.lunardepth_ports // {}) | to_entries[] | .value)
  ]
  | group_by(.) | map(select(length > 1)) | length
' "$tmp")"
health_metrics_conflict="$(jq -r --argjson lstart "$LUNARDEPTH_RANGE_START" '
  ((.health_metrics_port // 0) >= $lstart and (.health_metrics_port // 0) <= ($lstart + 999)) | tostring
' "$tmp")"
if [[ "$collisions" -ne 0 ]]; then
  echo "error: migration would create $collisions colliding port(s); registry unchanged" >&2
  echo "       inspect candidate: $tmp" >&2
  exit 1
fi
if [[ "$health_metrics_conflict" == "true" ]]; then
  echo "error: lunardepth range overlaps the health_metrics slot; set LUNARDEPTH_RANGE_START and retry" >&2
  rm -f "$tmp"
  exit 1
fi

chmod 0644 "$tmp"
mv "$tmp" "$PORTS_REGISTRY"
echo "port registry migrated to v18 (lunardepth range ${LUNARDEPTH_RANGE_START}-${range_end})"
echo "rollback if needed: cp $backup $PORTS_REGISTRY"
