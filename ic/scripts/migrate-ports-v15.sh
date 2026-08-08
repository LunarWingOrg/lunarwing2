#!/usr/bin/env bash
# Migrate the multi-tenant port registry from v14 to v15.
# Adds isolated LunarVoice service/health ports without changing existing ports.

set -euo pipefail

PORTS_REGISTRY="${PORTS_REGISTRY:-/etc/lunarwing/ports.json}"

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
if [[ "$current_version" -ge 15 ]]; then
  echo "already at v${current_version}, nothing to do"
  exit 0
fi
if [[ "$current_version" -lt 14 ]]; then
  echo "error: registry at v${current_version}; migrate through v14 first" >&2
  exit 1
fi

backup="${PORTS_REGISTRY}.bak.$(date -u +%Y%m%d%H%M%S)"
cp -p "$PORTS_REGISTRY" "$backup"
echo "backed up registry to $backup"

tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
jq '
  .version = 15
  | .voice_range = ((.voice_range) // {"start": 40000, "end": 49999})
  | .voice_block_size = ((.voice_block_size) // 2)
  | ( .range.start // 10000 ) as $rstart
  | ( .voice_range.start ) as $vstart
  | .tenants |= with_entries(
      .value |= (
        ( .base_port - $rstart + $vstart ) as $vbase
        | .enable_voice = ((.enable_voice) // false)
        | .voice_base = ((.voice_base) // $vbase)
        | .voice_ports = ((.voice_ports) // {
            voice_service: ($vbase + 0),
            voice_health: ($vbase + 1)
          })
      )
    )
' "$PORTS_REGISTRY" >"$tmp"

collisions="$(jq -r '
  [ .tenants[]
    | ((.ports // {}) | to_entries[] | .value),
      ((.extended_ports // {}) | to_entries[] | .value),
      ((.kimi_ports // {}) | to_entries[] | .value),
      ((.voice_ports // {}) | to_entries[] | .value)
  ]
  | group_by(.) | map(select(length > 1)) | length
' "$tmp")"
if [[ "$collisions" -ne 0 ]]; then
  echo "error: migration would create $collisions colliding port(s); registry unchanged" >&2
  echo "       inspect candidate: $tmp" >&2
  exit 1
fi

chmod 0644 "$tmp"
mv "$tmp" "$PORTS_REGISTRY"
echo "port registry migrated to v15 (LunarVoice service + health ports)"
echo "rollback if needed: cp $backup $PORTS_REGISTRY"
