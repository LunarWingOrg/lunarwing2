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
    [[ -n "$home" ]] || die "cannot resolve home for tenant '$tenant'"
    (cd / && exec sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
      "$PODMAN" "$@")
  else
    "$PODMAN" "$@"
  fi
}

network_tenant="${chain#fwd_}"
network_tenant="${network_tenant//_/-}"
[[ "$network_tenant" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
  || die "invalid tenant derived from worker egress chain '$chain'"
[[ -z "$tenant" || "$tenant" == "$network_tenant" ]] \
  || die "tenant and forward chain do not match"
network="lw-egress-$network_tenant"
keeper="lw-egress-keeper-$network_tenant"
keeper_infra=""
keeper_image=""
keeper_state="not applicable (rootful)"

keeper_exists() {
  local rc
  podman_for_tenant pod exists "$keeper" >/dev/null 2>&1 && return 0
  rc=$?
  [[ "$rc" -eq 1 ]] || die "cannot inspect namespace keeper $keeper"
  return 1
}

remove_untrusted_keeper() {
  local image="$1"
  podman_for_tenant pod rm -f "$keeper" >/dev/null 2>&1 \
    || die "namespace keeper $keeper uses unapproved infra image '$image' and could not be removed"
  die "namespace keeper $keeper uses unapproved infra image '$image'; pod removed"
}

keeper_uses_native_pause() {
  local uid shape
  if [[ -n "$tenant" ]]; then
    uid="$(id -u -- "$tenant")" || return 1
  else
    uid="$(id -u)" || return 1
  fi
  shape="$(podman_for_tenant inspect --format '{{.Image}}|{{.Path}}|{{json .Args}}|{{json .Mounts}}|{{.Rootfs}}' "$keeper_infra")" \
    || return 1
  [[ "$shape" == "|/catatonit|[\"-P\"]|[]|/run/user/$uid/libpod/tmp/infra-container" ]]
}

validate_keeper() {
  local purpose count is_infra networks
  purpose="$(podman_for_tenant pod inspect --format '{{index .Labels "lunarwing.purpose"}}' "$keeper")" \
    || die "cannot inspect namespace keeper $keeper"
  [[ "$purpose" == worker-egress-keeper ]] \
    || die "refusing foreign keeper pod $keeper"
  count="$(podman_for_tenant pod inspect --format '{{.NumContainers}}' "$keeper")" \
    || die "cannot inspect container count for namespace keeper $keeper"
  [[ "$count" == 1 ]] || die "namespace keeper $keeper has unexpected container count '$count'"
  keeper_infra="$(podman_for_tenant pod inspect --format '{{.InfraContainerID}}' "$keeper")" \
    || die "cannot inspect keeper infra container $keeper"
  [[ -n "$keeper_infra" ]] || die "keeper pod $keeper has no infra container"
  is_infra="$(podman_for_tenant inspect --format '{{.IsInfra}}' "$keeper_infra")" \
    || die "cannot inspect infra status for keeper $keeper"
  [[ "$is_infra" == true ]] || die "keeper $keeper is not a pause infra container"
  keeper_image="$(podman_for_tenant inspect --format '{{.ImageName}}' "$keeper_infra")" \
    || die "cannot inspect infra image for keeper $keeper"
  if [[ "$keeper_image" != localhost/podman-pause:* || "$keeper_image" == localhost/podman-pause: ]]; then
    remove_untrusted_keeper "$keeper_image"
  fi
  networks="$(podman_for_tenant inspect --format '{{range $name, $_ := .NetworkSettings.Networks}}{{printf "%s\n" $name}}{{end}}' "$keeper_infra")" \
    || die "cannot inspect keeper network $keeper"
  [[ "$networks" == "$network" ]] \
    || die "keeper $keeper is not attached exclusively to $network"
}

ensure_keeper() {
  local running
  if ! keeper_exists; then
    podman_for_tenant pod create --name "$keeper" --network "$network" --dns 127.0.0.1 \
      --label "lunarwing.tenant=$network_tenant" --label lunarwing.purpose=worker-egress-keeper >/dev/null \
      || keeper_exists \
      || die "cannot create rootless namespace keeper $keeper (refusing unfiltered start)"
  fi
  validate_keeper
  if ! podman_for_tenant pod start "$keeper" >/dev/null 2>&1; then
    running="$(podman_for_tenant inspect --format '{{.State.Running}}' "$keeper_infra")" \
      || die "cannot inspect keeper state $keeper"
    [[ "$running" == true ]] || die "cannot start rootless namespace keeper $keeper"
  fi
  running="$(podman_for_tenant inspect --format '{{.State.Running}}' "$keeper_infra")" \
    || die "cannot inspect keeper state $keeper"
  [[ "$running" == true ]] || die "rootless namespace keeper $keeper is not running"
  keeper_state=yes
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
  dump="$("$NFT" list chain inet lunarwing_worker_egress "$chain_name" 2>/dev/null)" \
    || die "egress chain $chain_name did not survive (namespace read: $netns; keeper running: $keeper_state)" 2
  awk -v subnet="$subnet" '
    $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $5 == subnet {
      if ($6 == "counter" && $7 == "reject" && $8 == "with" && $9 == "icmpx" && (($10 == "type" && $11 == "admin-prohibited") || ($10 == "admin-prohibited"))) found = 1
      if ($6 == "counter" && $7 == "packets" && $11 == "reject" && $12 == "with" && $13 == "icmpx" && (($14 == "type" && $15 == "admin-prohibited") || ($14 == "admin-prohibited"))) found = 1
    }
    END { exit(found ? 0 : 1) }
  ' <<<"$dump" || {
    printf '%s\n' "$dump" >&2
    die "egress chain $chain_name lacks the tenant's exact reject for $subnet (namespace read: $netns; keeper running: $keeper_state)" 2
  }
}

filter_session() {
  set -euo pipefail
  local NFT="$1" ruleset="$2" chain="$3" input_chain="$4" subnet="$5" verify_only="$6" keeper_state="$7" netns
  netns="$(readlink /proc/self/ns/net 2>/dev/null || printf unknown)"
  if [[ "$verify_only" != true ]] && ! "$NFT" -f "$ruleset"; then
    die "failed to load the egress ruleset $ruleset (namespace read: $netns; keeper running: $keeper_state)" 1
  fi
  verify_chain "$chain"
  verify_chain "$input_chain"
  printf 'worker-egress-guard: filter live (%s + %s, subnet %s, namespace %s; keeper running: %s)\n' \
    "$chain" "$input_chain" "$subnet" "$netns" "$keeper_state"
}

if [[ "$mode" == rootless ]]; then
  if [[ "$VERIFY_ONLY" != true ]]; then
    ensure_keeper
  elif keeper_exists; then
    validate_keeper
    running="$(podman_for_tenant inspect --format '{{.State.Running}}' "$keeper_infra")" \
      || die "cannot inspect keeper state $keeper"
    [[ "$running" == true ]] && keeper_state=yes || keeper_state=no
  else
    keeper_state=no
  fi
fi

session="$(declare -f die verify_chain filter_session)"$'\n''filter_session "$@"'
session_path="$(dirname "$NFT"):/usr/sbin:/usr/bin:/sbin:/bin"
if [[ "$mode" == rootless ]]; then
  if podman_for_tenant unshare --rootless-netns env -i "PATH=$session_path" HOME=/ \
      bash --noprofile --norc -c "$session" bash "$NFT" "$ruleset" "$chain" "$input_chain" \
      "$subnet" "$VERIFY_ONLY" "$keeper_state"; then
    exit 0
  else
    session_rc=$?
    die "rootless filter session failed (namespace read: unavailable; keeper running: $keeper_state)" "$session_rc"
  fi
fi
if env -i "PATH=$session_path" HOME=/ bash --noprofile --norc -c "$session" bash \
    "$NFT" "$ruleset" "$chain" "$input_chain" "$subnet" "$VERIFY_ONLY" "$keeper_state"; then
  exit 0
else
  session_rc=$?
  die "filter session failed (namespace read: unavailable; keeper running: $keeper_state)" "$session_rc"
fi
