#!/usr/bin/env bash
# WI#424 control 1 — load/verify a tenant's worker egress boundary.
# Usage: lunarwing-worker-egress-guard [--verify] <ruleset> <rootless|rootful> <chain> [tenant]
set -euo pipefail

TABLE="lunarwing_worker_egress"
VERIFY_ONLY=false
if [[ "${1:-}" == "--verify" ]]; then
  VERIFY_ONLY=true
  shift
fi

die() { printf 'worker-egress-guard: %s\n' "$1" >&2; exit "${2:-3}"; }

[[ $# -ge 3 && $# -le 4 ]] \
  || die "usage: $0 [--verify] <ruleset> <rootless|rootful> <chain> [tenant]"
ruleset="$1"
mode="$2"
chain="$3"
tenant="${4:-}"
case "$mode" in
  rootless|rootful) ;;
  *) die "invalid mode '$mode' (expected rootless or rootful)" ;;
esac
[[ "$chain" =~ ^worker-[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
  || die "invalid worker egress chain '$chain'"
if [[ -n "$tenant" ]]; then
  [[ "$tenant" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
    || die "invalid tenant name '$tenant'"
fi
input_chain="worker-input-${chain#worker-}"
[[ -f "$ruleset" && ! -L "$ruleset" ]] \
  || die "egress ruleset not found or not a regular file: $ruleset"

NFT="$(command -v nft 2>/dev/null)" || NFT=""
[[ -n "$NFT" ]] || die "nft not found; refusing to start a worker without an egress filter"
PODMAN="$(command -v podman 2>/dev/null || printf '/usr/bin/podman')"

nft_in_netns() {
  if [[ "$mode" != "rootless" ]]; then
    "$NFT" "$@"
    return
  fi
  if [[ -n "$tenant" && "$(id -u)" == "0" ]]; then
    local uid home
    uid="$(id -u -- "$tenant")" || die "cannot resolve uid for tenant '$tenant'"
    home="$(getent passwd "$tenant" | cut -d: -f6)"
    (cd / && exec sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
      "$PODMAN" unshare --rootless-netns "$NFT" "$@")
    return
  fi
  "$PODMAN" unshare --rootless-netns "$NFT" "$@"
}

# Only the generated all-protocol reject yields the expected subnet. A TCP-only
# or missing reject never reaches the load path.
subnet="$(awk '
  $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $6 == "counter" &&
  $7 == "reject" && $8 == "with" && $9 == "icmpx" && $10 == "type" && $11 == "admin-prohibited" {
    if (!found) { subnet = $5; found = 1 }
  }
  END { if (!found) exit 1; print subnet }
' "$ruleset")" \
  || die "ruleset has no exact all-protocol reject; refusing to start unfiltered" 2

verify_chain() {
  local chain_name="$1" dump
  dump="$(nft_in_netns list chain inet "$TABLE" "$chain_name" 2>/dev/null)" \
    || die "egress chain $chain_name did not survive (refusing unfiltered start)" 2
  awk -v subnet="$subnet" '
    $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $5 == subnet {
      if ($6 == "counter" && $7 == "reject" && $8 == "with" && $9 == "icmpx" && $10 == "type" && $11 == "admin-prohibited") found = 1
      if ($6 == "counter" && $7 == "packets" && $11 == "reject" && $12 == "with" && $13 == "icmpx" && $14 == "type" && $15 == "admin-prohibited") found = 1
    }
    END { exit(found ? 0 : 1) }
  ' <<<"$dump" \
    || die "egress chain $chain_name lacks the tenant's exact reject for $subnet" 2
}

if [[ "$VERIFY_ONLY" != "true" ]]; then
  nft_in_netns -f "$ruleset" >/dev/null 2>&1 \
    || die "failed to load the egress ruleset $ruleset" 1
fi
verify_chain "$chain"
verify_chain "$input_chain"
printf 'worker-egress-guard: filter live (%s, %s + %s, subnet %s)\n' \
  "$mode" "$chain" "$input_chain" "$subnet"
