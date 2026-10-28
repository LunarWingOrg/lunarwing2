#!/usr/bin/env bash
# WI#424: read-only, host-side worker egress audit for the health pipeline.
#
# This runs as root from cron, so it must never execute or source a file from
# an operator checkout (the maintainer's 2026-10-15 rule; see OQ-1 of the
# !1945 run-226 review). Every read below is either a root-owned state file
# under /etc/lunarwing/worker-egress/ or a container-runtime/nft observation
# made as the tenant; nothing is taken from the provisioning tree.
set -uo pipefail

TENANTS_FILE="${SELF_HEAL_TENANTS_FILE:-${LUNARWING_TENANTS_FILE:-/etc/lunarwing/ports.json}}"
AUDIT_TIMEOUT="${HEALTH_WORKER_EGRESS_TIMEOUT:-8}"
EGRESS_DIR="${LUNARWING_MT_WORKER_EGRESS_DIR:-/etc/lunarwing/worker-egress}"
TABLE="lunarwing_worker_egress"
# DNS enforcement is a podman-network property. Docker's embedded resolver
# always runs, so its DNS state is not verifiable here; the audit reports it as
# unverified rather than claiming `disabled` (OQ-5).
RUNTIME="${HEALTH_WORKER_EGRESS_RUNTIME:-auto}"
ROOTLESS="${HEALTH_WORKER_EGRESS_ROOTLESS:-auto}"
instances=()
issues=()

json_array() {
  if [[ $# -eq 0 ]]; then
    printf '[]\n'
  else
    printf '%s\n' "$@" | jq -s .
  fi
}

json_issues() {
  if [[ $# -eq 0 ]]; then
    printf '[]\n'
  else
    printf '%s\n' "$@" | jq -R . | jq -s .
  fi
}

report() {
  local status="$1" rc="$2"
  jq -n \
    --arg component "worker_egress" \
    --arg status "$status" \
    --arg timestamp "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
    --argjson instances "$(json_array "${instances[@]}")" \
    --argjson issues "$(json_issues "${issues[@]}")" \
    '{component: $component, status: $status, timestamp: $timestamp,
      metrics: {instances: $instances}, issues: $issues}'
  exit "$rc"
}

if ! command -v jq >/dev/null 2>&1; then
  printf '{"component":"worker_egress","status":"degraded","metrics":{"instances":[]},"issues":["jq is unavailable"]}\n'
  exit 1
fi

if [[ ! -r "$TENANTS_FILE" ]]; then
  report healthy 0
fi
if ! tenants="$(jq -er '
  .tenants // {} | to_entries[]
  | select((.value.workers.pebble == true) or (.value.workers.opencode == true)
      or (.value.workers.opencode2 == true) or (.value.workers.kimi == true))
  | .key
' "$TENANTS_FILE" 2>/dev/null)"; then
  issues+=("tenant registry is invalid; worker egress coverage is unknown")
  report degraded 1
fi

detect_runtime() {
  [[ "$RUNTIME" != "auto" ]] && { printf '%s\n' "$RUNTIME"; return 0; }
  if command -v podman >/dev/null 2>&1; then
    printf 'podman'; return 0
  fi
  if command -v docker >/dev/null 2>&1; then
    printf 'docker'; return 0
  fi
  return 1
}

state_get() {  # <tenant> <KEY>
  local conf="$EGRESS_DIR/$1.conf" key="$2" line
  [[ -f "$conf" && ! -L "$conf" ]] || return 0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" == "$key="* ]] && printf '%s\n' "${line#*=}"
  done <"$conf"
  return 0
}

normalize_name() { printf '%s' "${1//-/_}"; }

# Bridge and gateway are derived exactly as mt-admin derives them, from the
# recorded subnet alone.
bridge_for_subnet() {
  local third
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.([0-9]+)\.0/24$ ]] || return 1
  third="${BASH_REMATCH[1]}"
  ((third >= 10 && third <= 250)) || return 1
  printf 'lweg%s\n' "$third"
}

gateway_for_subnet() {
  local net="${1%%/*}"
  printf '%s.1\n' "${net%.*}"
}

bounded() {  # <cmd...> — per-observation timeout
  if command -v timeout >/dev/null 2>&1; then
    timeout -k 2 "$AUDIT_TIMEOUT" "$@"
  else
    "$@"
  fi
}

as_tenant() {  # <tenant> <cmd...> — read-only runtime observation as the tenant
  local tenant="$1"; shift
  local uid home
  uid="$(id -u "$tenant" 2>/dev/null)" || return 1
  home="$(getent passwd "$tenant" | cut -d: -f6)"
  ( cd / && exec timeout -k 2 "$AUDIT_TIMEOUT" \
      sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" "$@" )
}

nft_read() {  # <tenant> <rootless|rootful> <args...>
  local tenant="$1" mode="$2"; shift 2
  if [[ "$mode" == "rootless" ]]; then
    as_tenant "$tenant" podman unshare --rootless-netns nft "$@"
  else
    bounded nft "$@"
  fi
}

runtime_read() {  # <tenant> <args...>
  local tenant="$1"; shift
  if [[ "$ROOTLESS" == "true" ]]; then
    as_tenant "$tenant" "$RUNTIME" "$@"
  else
    bounded "$RUNTIME" "$@"
  fi
}

network_subnet_bridge_gateway() {  # <tenant> <network> — prints subnet, bridge, gateway
  local tenant="$1" network="$2" json
  json="$(runtime_read "$tenant" network inspect "$network" 2>/dev/null)" || return 1
  python3 -I -c '
import json, sys
doc = json.load(sys.stdin)
net = doc[0] if isinstance(doc, list) else doc
def ci(obj, key, default=None):
    for candidate, value in obj.items():
        if candidate.lower() == key.lower():
            return value
    return default
subnet = gateway = ""
for item in ci(net, "subnets", []) or []:
    if isinstance(item, dict):
        subnet = subnet or ci(item, "subnet", "")
        gateway = gateway or ci(item, "gateway", "")
if not subnet:
    ipam = ci(net, "ipam", {}) or {}
    for item in ci(ipam, "config", []) or []:
        subnet = subnet or ci(item, "subnet", "")
        gateway = gateway or ci(item, "gateway", "")
options = ci(net, "options", {}) or {}
interface = ci(net, "network_interface", "") or ci(options, "com.docker.network.bridge.name", "")
print(subnet)
print(interface)
print(gateway)
' <<<"$json"
}

container_attached() {  # <tenant> <network> <container>
  local tenant="$1" network="$2" container="$3" networks dns hosts mapping
  # shellcheck disable=SC2016  # $name is a Go-template key, not a shell expansion.
  networks="$(runtime_read "$tenant" inspect --format '{{range $name, $_ := .NetworkSettings.Networks}}{{printf "%s\n" $name}}{{end}}' \
    "$container" 2>/dev/null)" || return 1
  [[ "$networks" == "$network" ]] || return 1
  dns="$(runtime_read "$tenant" inspect --format '{{range .HostConfig.Dns}}{{printf "%s\n" .}}{{end}}' \
    "$container" 2>/dev/null)" || return 1
  [[ "$dns" == "127.0.0.1" ]] || return 1
  hosts="$(runtime_read "$tenant" inspect --format '{{range .HostConfig.ExtraHosts}}{{printf "%s\n" .}}{{end}}' \
    "$container" 2>/dev/null)" || return 1
  local hosts_file="$EGRESS_DIR/$tenant.hosts"
  [[ -f "$hosts_file" && ! -L "$hosts_file" ]] || return 1
  while IFS= read -r mapping || [[ -n "$mapping" ]]; do
    [[ -n "$mapping" ]] || continue
    grep -Fqx -- "$mapping" <<<"$hosts" || return 1
  done <"$hosts_file"
}

running_worker_containers() {  # <tenant> — prints each running managed worker container
  local tenant="$1" container state
  for container in "lunarwing-pebble-$tenant" "lunarwing-opencode-$tenant" \
                   "lunarwing-opencode2-$tenant" "lunarwing-kimi-$tenant" \
                   "lunarwing-pebble-task-$tenant" "lunarwing-opencode-task-$tenant" \
                   "lunarwing-opencode2-task-$tenant" "lunarwing-kimi-task-$tenant"; do
    state="$(runtime_read "$tenant" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" || continue
    [[ "$state" == "true" ]] && printf '%s\n' "$container"
  done
  return 0
}

note() { printf 'ISSUE:%s\n' "$1"; }  # buffered for the parent process

audit_tenant() {  # <tenant> — returns 0 healthy, 1 missing; issues on stdout
  local tenant="$1" network subnet bridge gateway mode chain dump missing=0
  local -a running=()
  network="lw-egress-$tenant"
  subnet="$(state_get "$tenant" SUBNET)"
  if [[ -z "$subnet" ]]; then
    note "worker egress is not configured for $tenant (no egress network allocated)"
    return 1
  fi
  bridge="$(bridge_for_subnet "$subnet")" || {
    note "worker egress records an invalid subnet for $tenant: $subnet"
    return 1
  }
  gateway="$(gateway_for_subnet "$subnet")"
  if [[ "$RUNTIME" == "podman" && "$ROOTLESS" == "true" ]]; then
    mode=rootless
  else
    mode=rootful
  fi
  # Network contract: subnet, bridge name and (podman only) disabled DNS.
  local details d_subnet d_bridge d_gateway
  local -a _fields=()
  if details="$(network_subnet_bridge_gateway "$tenant" "$network")"; then
    mapfile -t _fields <<<"$details"
    d_subnet="${_fields[0]:-}"; d_bridge="${_fields[1]:-}"; d_gateway="${_fields[2]:-}"
    [[ "$d_subnet" == "$subnet" ]] || {
      note "worker egress network $network has subnet '${d_subnet:-none}', expected $subnet"
      missing=1
    }
    [[ "$d_bridge" == "$bridge" ]] || {
      note "worker egress network $network has bridge '${d_bridge:-none}', expected $bridge"
      missing=1
    }
    [[ "$d_gateway" == "$gateway" ]] || {
      note "worker egress network $network has gateway '${d_gateway:-none}', expected $gateway"
      missing=1
    }
    if [[ "$RUNTIME" == "podman" ]]; then
      local dns_state
      dns_state="$(runtime_read "$tenant" network inspect "$network" 2>/dev/null \
        | python3 -I -c 'import json,sys
doc=json.load(sys.stdin); net=doc[0] if isinstance(doc,list) else doc
def ci(o,k,d=None):
    for c,v in o.items():
        if c.lower()==k.lower(): return v
    return d
print("disabled" if ci(net,"dns_enabled",None) is False else "enabled")' 2>/dev/null)"
      [[ "$dns_state" == "disabled" ]] || {
        note "worker egress network $network still has recursive DNS ${dns_state:-unknown}"
        missing=1
      }
    fi
  else
    note "worker egress network $network is missing for $tenant"
    missing=1
  fi
  # Container attachment and static hosts.
  while IFS= read -r container; do
    [[ -n "$container" ]] || continue
    running+=("$container")
    if ! container_attached "$tenant" "$network" "$container"; then
      note "worker container $container for $tenant is unfiltered or has a stale DNS/hosts contract"
      missing=1
    fi
  done < <(running_worker_containers "$tenant")
  # Live rules. Only read the namespace when a container pins it: an idle
  # tenant must not have a rootless netns created just to audit it (OQ-6).
  if ((${#running[@]} > 0)); then
    for chain in "fwd_$(normalize_name "$tenant")" "in_$(normalize_name "$tenant")"; do
      if ! dump="$(nft_read "$tenant" "$mode" list chain inet "$TABLE" "$chain" 2>/dev/null)"; then
        note "worker egress chain $chain is not loaded for $tenant"
        missing=1
        continue
      fi
      if ! awk -v subnet="$subnet" '
        $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $5 == subnet {
          if ($6 == "counter" && $7 == "reject" && $8 == "with" && $9 == "icmpx" && (($10 == "type" && $11 == "admin-prohibited") || ($10 == "admin-prohibited"))) found = 1
          if ($6 == "counter" && $7 == "packets" && $11 == "reject" && $12 == "with" && $13 == "icmpx" && (($14 == "type" && $15 == "admin-prohibited") || ($14 == "admin-prohibited"))) found = 1
        }
        END { exit(found ? 0 : 1) }
      ' <<<"$dump"; then
        note "worker egress chain $chain for $tenant lacks the exact reject for $subnet"
        missing=1
      fi
    done
  fi
  return "$missing"
}

if ! detected="$(detect_runtime)"; then
  issues+=("neither podman nor docker is available; worker egress cannot be audited")
  report degraded 1
fi
RUNTIME="$detected"
if [[ "$ROOTLESS" == "auto" ]]; then
  [[ "$RUNTIME" == "podman" ]] && ROOTLESS=true || ROOTLESS=false
fi

while IFS= read -r tenant; do
  [[ -n "$tenant" ]] || continue
  tenant_rc=0
  # Each runtime/nft observation is bounded by `bounded`, so a wedged tenant
  # cannot eat the orchestrator budget without needing a re-exec.
  tenant_output="$(audit_tenant "$tenant")" || tenant_rc=$?
  while IFS= read -r line; do
    [[ "$line" == ISSUE:* ]] && issues+=("${line#ISSUE:}")
  done <<<"$tenant_output"
  if [[ "$tenant_rc" == "0" ]]; then
    instances+=("$(jq -n --arg tenant "$tenant" '{tenant: $tenant, status: "healthy"}')")
  else
    instances+=("$(jq -n --arg tenant "$tenant" '{tenant: $tenant, status: "critical"}')")
  fi
done <<<"$tenants"

for instance in "${instances[@]}"; do
  [[ "$(jq -r '.status' <<<"$instance")" != "critical" ]] || report critical 2
done
report healthy 0
