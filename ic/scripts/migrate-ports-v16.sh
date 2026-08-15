#!/usr/bin/env bash
# Migrate the multi-tenant port registry from v15 to v16.
# Adds a host-global `health_metrics_port` for the optional loopback-only
# healthpoint metrics listener. No tenant port changes.

set -euo pipefail

PORTS_REGISTRY="${PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
# Optional override so operators can pin a different host-global slot.
HEALTH_METRICS_PORT="${HEALTH_METRICS_PORT:-50000}"

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
if [[ "$current_version" -ge 16 ]]; then
  echo "already at v${current_version}, nothing to do"
  exit 0
fi
if [[ "$current_version" -lt 15 ]]; then
  echo "error: registry at v${current_version}; migrate through v15 first" >&2
  exit 1
fi

# Never silently move a slot an operator hand-pinned.
pinned="$(jq -r '.health_metrics_port // empty' "$PORTS_REGISTRY")"
port="${pinned:-$HEALTH_METRICS_PORT}"

backup="${PORTS_REGISTRY}.bak.$(date -u +%Y%m%d%H%M%S)"
cp -p "$PORTS_REGISTRY" "$backup"
echo "backed up registry to $backup"

tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
jq --argjson port "$port" '.version = 16 | .health_metrics_port = ((.health_metrics_port) // $port)' \
  "$PORTS_REGISTRY" >"$tmp"

collisions="$(jq -r '
  [ .tenants[]
    | ((.ports // {}) | to_entries[] | .value),
      ((.extended_ports // {}) | to_entries[] | .value),
      ((.kimi_ports // {}) | to_entries[] | .value),
      ((.voice_ports // {}) | to_entries[] | .value)
  ]
  | group_by(.) | map(select(length > 1)) | length
' "$tmp")"
slot_taken="$(jq -r --argjson p "$port" '
  [ .tenants[]
    | ((.ports // {}) | to_entries[] | .value),
      ((.extended_ports // {}) | to_entries[] | .value),
      ((.kimi_ports // {}) | to_entries[] | .value),
      ((.voice_ports // {}) | to_entries[] | .value)
  ] | any(. == $p)
' "$tmp")"
if [[ "$collisions" -ne 0 ]]; then
  echo "error: migration would create $collisions colliding port(s); registry unchanged" >&2
  echo "       inspect candidate: $tmp" >&2
  exit 1
fi
if [[ "$slot_taken" == "true" ]]; then
  echo "error: health_metrics port $port is already used by a tenant; set HEALTH_METRICS_PORT and retry" >&2
  exit 1
fi

chmod 0644 "$tmp"
mv "$tmp" "$PORTS_REGISTRY"
echo "port registry migrated to v16 (host-global health_metrics port: $port)"
echo "rollback if needed: cp $backup $PORTS_REGISTRY"
