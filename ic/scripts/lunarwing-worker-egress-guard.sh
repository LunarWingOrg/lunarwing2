#!/usr/bin/env bash
# WI#424 control 1 — load/verify a tenant's worker egress boundary.
# Usage: lunarwing-worker-egress-guard [--verify] <ruleset> <rootless|rootful> <forward-chain> [tenant]
set -euo pipefail

TABLE="lunarwing_worker_egress"
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

DUMP_TOKEN='##LWEGRESS##'

# Load and inspect both chains within one rootless-netns session. Without a live
# Podman netns keeper, every separate `unshare --rootless-netns` gets a fresh ns.
session_script() {
  cat <<'SESSION'
ruleset="$1"; shift
printf "%s %s\n" "$GUARD_DUMP_TOKEN" "@netns"
readlink /proc/self/ns/net 2>/dev/null || printf "unknown\n"
if [ -n "$ruleset" ]; then
  "$GUARD_NFT" -f "$ruleset" >&2 || exit 11
fi
for chain in "$@"; do
  printf "%s %s\n" "$GUARD_DUMP_TOKEN" "$chain"
  if ! "$GUARD_NFT" list chain inet "$GUARD_TABLE" "$chain" 2>/dev/null; then
    printf "%s %s\n" "$GUARD_DUMP_TOKEN" "@missing:$chain"
  fi
done
SESSION
}

netns_session() {  # <ruleset-to-load-or-empty> <chain>...
  local -a session_env=(
    "PATH=$PATH"
    "GUARD_NFT=$NFT"
    "GUARD_TABLE=$TABLE"
    "GUARD_DUMP_TOKEN=$DUMP_TOKEN"
  )
  local script
  script="$(session_script)"
  if [[ "$mode" != "rootless" ]]; then
    env -i "${session_env[@]}" bash -c "$script" bash "$@"
    return
  fi
  if [[ -n "$tenant" && "$(id -u)" == "0" ]]; then
    local uid home
    uid="$(id -u -- "$tenant")" || die "cannot resolve uid for tenant '$tenant'"
    home="$(getent passwd "$tenant" | cut -d: -f6)"
    (cd / && exec sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
      "$PODMAN" unshare --rootless-netns env -i "${session_env[@]}" \
      bash -c "$script" bash "$@")
    return
  fi
  "$PODMAN" unshare --rootless-netns env -i "${session_env[@]}" \
    bash -c "$script" bash "$@"
}

dump_section() {  # <combined-session-output> <section-name>
  awk -v token="$DUMP_TOKEN" -v want="$2" '
    substr($0, 1, length(token)) == token { cur = substr($0, length(token) + 2); next }
    cur == want { print }
  ' <<<"$1"
}

keeper_state() {
  local running=""
  if [[ "$mode" != "rootless" ]]; then
    printf 'not applicable (rootful nft)'
    return 0
  fi
  if [[ -n "$tenant" && "$(id -u)" == "0" ]]; then
    local uid home
    uid="$(id -u -- "$tenant" 2>/dev/null)" || { printf 'unknown'; return 0; }
    home="$(getent passwd "$tenant" | cut -d: -f6)"
    running="$(cd / && sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
      "$PODMAN" ps -q 2>/dev/null)" || running=""
  else
    running="$("$PODMAN" ps -q 2>/dev/null)" || running=""
  fi
  if [[ -n "${running//[[:space:]]/}" ]]; then
    printf 'running (containers up)'
  else
    printf 'absent (no container is up; each unshare invocation gets a fresh namespace)'
  fi
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

verify_dump() {  # <chain> <dump> <namespace>
  local chain_name="$1" dump="$2" netns="$3"
  awk -v subnet="$subnet" '
    $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $5 == subnet {
      if ($6 == "counter" && $7 == "reject" && $8 == "with" && $9 == "icmpx" && (($10 == "type" && $11 == "admin-prohibited") || ($10 == "admin-prohibited"))) found = 1
      if ($6 == "counter" && $7 == "packets" && $11 == "reject" && $12 == "with" && $13 == "icmpx" && (($14 == "type" && $15 == "admin-prohibited") || ($14 == "admin-prohibited"))) found = 1
    }
    END { exit(found ? 0 : 1) }
  ' <<<"$dump" || {
    printf '%s\n' "$dump" >&2
    die "egress chain $chain_name lacks the tenant's exact reject for $subnet (namespace read: $netns; podman rootless netns keeper: $(keeper_state))" 2
  }
}

session_ruleset=""
[[ "$VERIFY_ONLY" == "true" ]] || session_ruleset="$ruleset"
session_rc=0
session_out="$(netns_session "$session_ruleset" "$chain" "$input_chain")" || session_rc=$?
session_netns="$(dump_section "$session_out" "@netns")"
session_netns="${session_netns%%$'\n'*}"
[[ -n "$session_netns" ]] || session_netns="unknown"
if [[ "$session_rc" == "11" ]]; then
  die "failed to load the egress ruleset $ruleset in network namespace $session_netns (podman rootless netns keeper: $(keeper_state))" 1
fi
[[ "$session_rc" == "0" ]] \
  || die "cannot run nft in the network namespace (exit $session_rc; namespace $session_netns; podman rootless netns keeper: $(keeper_state))" 1

for chain_name in "$chain" "$input_chain"; do
  if [[ -n "$(dump_section "$session_out" "@missing:$chain_name")" ]]; then
    die "egress chain $chain_name did not survive (namespace read: $session_netns; podman rootless netns keeper: $(keeper_state))" 2
  fi
  verify_dump "$chain_name" "$(dump_section "$session_out" "$chain_name")" "$session_netns"
done
printf 'worker-egress-guard: filter live (%s, %s + %s, subnet %s, namespace %s)\n' \
  "$mode" "$chain" "$input_chain" "$subnet" "$session_netns"
