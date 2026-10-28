#!/usr/bin/env bash
# WI#424 control 1 — load/verify a tenant's worker egress boundary.
# Usage: lunarwing-worker-egress-guard [--verify] <ruleset> <rootless|rootful> <forward-chain> [tenant]
set -euo pipefail

VERIFY_ONLY=false
if [[ "${1:-}" == "--verify" ]]; then
  VERIFY_ONLY=true
  shift
fi

die() { printf 'worker-egress-guard: %s\n' "$1" >&2; exit "${2:-3}"; }

[[ $# -ge 3 && $# -le 4 ]] \
  || die "usage: $0 [--verify] <ruleset> <rootless|rootful> <forward-chain> [tenant]"
ruleset="$1"
mode="$2"
chain="$3"
tenant="${4:-}"
case "$mode" in
  rootless|rootful) ;;
  *) die "invalid mode '$mode' (expected rootless or rootful)" ;;
esac
[[ "$chain" =~ ^fwd_[a-z0-9]([a-z0-9_]*[a-z0-9])?$ ]] \
  || die "invalid worker egress forward chain '$chain'"
if [[ -n "$tenant" ]]; then
  [[ "$tenant" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
    || die "invalid tenant name '$tenant'"
fi
# OQ-4: the two chain families use distinct fixed prefixes, so deriving the
# input chain can never coincide with another tenant's forward chain.
input_chain="in_${chain#fwd_}"
[[ -f "$ruleset" && ! -L "$ruleset" ]] \
  || die "egress ruleset not found or not a regular file: $ruleset"

NFT="$(command -v nft 2>/dev/null)" || NFT=""
[[ -n "$NFT" ]] || die "nft not found; refusing to start a worker without an egress filter"
PODMAN="$(command -v podman 2>/dev/null || printf '/usr/bin/podman')"

podman_for_tenant() {
  if [[ -n "$tenant" && "$(id -u)" == "0" ]]; then
    local uid home
    uid="$(id -u -- "$tenant")" || die "cannot resolve uid for tenant '$tenant'"
    home="$(getent passwd "$tenant" | cut -d: -f6)"
    (cd / && exec sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
      "$PODMAN" "$@")
  else
    "$PODMAN" "$@"
  fi
}

ensure_keeper() {
  local purpose infra networks running
  if ! podman_for_tenant pod exists "$keeper"; then
    podman_for_tenant pod create --name "$keeper" --network "$network" --dns 127.0.0.1 \
      --label "lunarwing.tenant=$network_tenant" --label lunarwing.purpose=worker-egress-keeper >/dev/null \
      || podman_for_tenant pod exists "$keeper" \
      || die "cannot create rootless namespace keeper $keeper (refusing unfiltered start)"
  fi
  purpose="$(podman_for_tenant pod inspect --format '{{index .Labels "lunarwing.purpose"}}' "$keeper")" \
    || die "cannot inspect namespace keeper $keeper"
  [[ "$purpose" == worker-egress-keeper ]] || die "refusing foreign keeper pod $keeper"
  infra="$(podman_for_tenant pod inspect --format '{{.InfraContainerID}}' "$keeper")" \
    || die "cannot inspect keeper infra container $keeper"
  [[ -n "$infra" ]] || die "keeper pod $keeper has no infra container"
  [[ "$(podman_for_tenant inspect --format '{{.IsInfra}}' "$infra")" == true ]] \
    || die "keeper $keeper is not a pause infra container"
  podman_for_tenant pod start "$keeper" >/dev/null \
    || [[ "$(podman_for_tenant inspect --format '{{.State.Running}}' "$infra")" == true ]] \
    || die "cannot start rootless namespace keeper $keeper"
  running="$(podman_for_tenant inspect --format '{{.State.Running}}' "$infra")" \
    || die "cannot inspect keeper state $keeper"
  networks="$(podman_for_tenant inspect --format '{{range $name, $_ := .NetworkSettings.Networks}}{{printf "%s\n" $name}}{{end}}' "$infra")" \
    || die "cannot inspect keeper network $keeper"
  [[ "$running" == true && "$networks" == "$network" ]] \
    || die "keeper $keeper is not running exclusively on $network"
}

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
  dump="$("$nft" list chain inet lunarwing_worker_egress "$chain_name" 2>/dev/null)" \
    || die "egress chain $chain_name did not survive the load (namespace read: $netns; keeper: $keeper_state)" 2
  awk -v subnet="$subnet" '
    $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $5 == subnet {
      if ($6 == "counter" && $7 == "reject" && $8 == "with" && $9 == "icmpx" && (($10 == "type" && $11 == "admin-prohibited") || ($10 == "admin-prohibited"))) found = 1
      if ($6 == "counter" && $7 == "packets" && $11 == "reject" && $12 == "with" && $13 == "icmpx" && (($14 == "type" && $15 == "admin-prohibited") || ($14 == "admin-prohibited"))) found = 1
    }
    END { exit(found ? 0 : 1) }
  ' <<<"$dump" || {
    printf '%s\n' "$dump" >&2
    die "egress chain $chain_name lacks the tenant's exact reject for $subnet (namespace read: $netns; keeper: $keeper_state)" 2
  }
  printf '%s\n' "$dump"
}

filter_session() {
  set -euo pipefail
  local nft="$1" ruleset="$2" chain="$3" input_chain="$4" subnet="$5" verify_only="$6" keeper_state="$7" netns
  netns="$(readlink /proc/self/ns/net 2>/dev/null || printf unknown)"
  if [[ "$verify_only" != true ]]; then
    "$nft" -f "$ruleset" \
      || die "failed to load the egress ruleset $ruleset (namespace read: $netns; keeper: $keeper_state)" 1
  fi
  verify_chain "$chain"
  verify_chain "$input_chain"
  printf 'worker-egress-guard: filter live (%s + %s, subnet %s, namespace %s; keeper: %s)\n' \
    "$chain" "$input_chain" "$subnet" "$netns" "$keeper_state"
}

network_tenant="${chain#fwd_}"
network_tenant="${network_tenant//_/-}"
[[ -z "$tenant" || "$tenant" == "$network_tenant" ]] || die "tenant and forward chain do not match"
network="lw-egress-$network_tenant"
keeper="lw-egress-keeper-$network_tenant"
keeper_state="not applicable (rootful)"
if [[ "$mode" == rootless ]]; then
  if [[ "$VERIFY_ONLY" != true ]]; then
    ensure_keeper
  fi
  keeper_state="$(podman_for_tenant pod inspect --format '{{.State}}' "$keeper" 2>/dev/null || printf absent)"
  keeper_state="$keeper $keeper_state"
fi
session="$(declare -f die verify_chain filter_session)"$'\n''filter_session "$@"'
session_path="$(dirname "$NFT"):/usr/sbin:/usr/bin:/sbin:/bin"
if [[ "$mode" == rootless ]]; then
  podman_for_tenant unshare --rootless-netns env -i "PATH=$session_path" HOME=/ \
    bash --noprofile --norc -c "$session" bash "$NFT" "$ruleset" "$chain" "$input_chain" "$subnet" "$VERIFY_ONLY" "$keeper_state"
else
  env -i "PATH=$session_path" HOME=/ bash --noprofile --norc -c "$session" bash \
    "$NFT" "$ruleset" "$chain" "$input_chain" "$subnet" "$VERIFY_ONLY" "$keeper_state"
fi
