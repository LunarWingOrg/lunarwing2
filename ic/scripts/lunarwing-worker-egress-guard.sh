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

# WI#504: one rootless network namespace for the whole load-and-verify.
#
# `podman unshare --rootless-netns` hands out a FRESH namespace on every
# invocation whenever podman's rootless netns keeper is not running — that is,
# while no bridge-networked container of the user is up (a tenant's first worker
# start, or any start after a reboot). Loading the ruleset in one invocation and
# reading it back in the next therefore verified an empty namespace and refused
# a correct ruleset. The session script below performs the load and both chain
# reads inside a single `podman unshare --rootless-netns`, so they cannot be
# split across namespaces.
SESSION_SCRIPT='
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
'

netns_session() {  # <ruleset-to-load-or-empty> <chain>...
  local -a session_env=(
    "GUARD_NFT=$NFT"
    "GUARD_TABLE=$TABLE"
    "GUARD_DUMP_TOKEN=$DUMP_TOKEN"
  )
  if [[ "$mode" != "rootless" ]]; then
    env "${session_env[@]}" bash -c "$SESSION_SCRIPT" bash "$@"
    return
  fi
  if [[ -n "$tenant" && "$(id -u)" == "0" ]]; then
    local uid home
    uid="$(id -u -- "$tenant")" || die "cannot resolve uid for tenant '$tenant'"
    home="$(getent passwd "$tenant" | cut -d: -f6)"
    (cd / && exec sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
      "$PODMAN" unshare --rootless-netns env "${session_env[@]}" \
      bash -c "$SESSION_SCRIPT" bash "$@")
    return
  fi
  "$PODMAN" unshare --rootless-netns env "${session_env[@]}" \
    bash -c "$SESSION_SCRIPT" bash "$@"
}

dump_section() {  # <combined-session-output> <section-name>
  awk -v token="$DUMP_TOKEN" -v want="$2" '
    substr($0, 1, length(token)) == token { cur = substr($0, length(token) + 2); next }
    cur == want { print }
  ' <<<"$1"
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

# WI#504: whether podman's rootless netns keeper can be running at all. The
# keeper only lives while a container of that user is up, which is exactly the
# state that used to decide whether load and verify shared a namespace.
keeper_state() {
  local running=""
  if [[ -n "$tenant" && "$(id -u)" == "0" ]]; then
    local uid home
    uid="$(id -u -- "$tenant" 2>/dev/null)" || { printf 'unknown'; return; }
    home="$(getent passwd "$tenant" | cut -d: -f6)"
    running="$(cd / && sudo -u "$tenant" env HOME="$home" \
      XDG_RUNTIME_DIR="/run/user/$uid" "$PODMAN" ps -q 2>/dev/null)" || running=""
  else
    running="$("$PODMAN" ps -q 2>/dev/null)" || running=""
  fi
  if [[ -n "${running//[[:space:]]/}" ]]; then
    printf 'running (containers up)'
  else
    printf 'none (no container of this user is up, so every podman unshare --rootless-netns invocation gets a fresh namespace)'
  fi
}

verify_dump() {  # <chain-name> <dump> <netns>
  local chain_name="$1" dump="$2" netns="$3"
  awk -v subnet="$subnet" '
    $1 == "iifname" && $3 == "ip" && $4 == "saddr" && $5 == subnet {
      if ($6 == "counter" && $7 == "reject" && $8 == "with" && $9 == "icmpx" && (($10 == "type" && $11 == "admin-prohibited") || ($10 == "admin-prohibited"))) found = 1
      if ($6 == "counter" && $7 == "packets" && $11 == "reject" && $12 == "with" && $13 == "icmpx" && (($14 == "type" && $15 == "admin-prohibited") || ($14 == "admin-prohibited"))) found = 1
    }
    END { exit(found ? 0 : 1) }
  ' <<<"$dump" || {
    printf '%s\n' "$dump" >&2
    die "egress chain $chain_name lacks the tenant's exact reject for $subnet \
(namespace read: $netns; podman rootless netns keeper: $(keeper_state))" 2
  }
}

session_ruleset=""
[[ "$VERIFY_ONLY" == "true" ]] || session_ruleset="$ruleset"
session_rc=0
session_out="$(netns_session "$session_ruleset" "$chain" "$input_chain")" || session_rc=$?
if [[ "$session_rc" != "0" ]]; then
  if [[ "$session_rc" == "11" ]]; then
    die "failed to load the egress ruleset $ruleset" 1
  fi
  die "cannot run nft in the rootless network namespace for $ruleset (exit $session_rc)" 1
fi
session_netns="$(dump_section "$session_out" "@netns")"
session_netns="${session_netns%%$'\n'*}"
[[ -n "$session_netns" ]] || session_netns="unknown"
for chain_name in "$chain" "$input_chain"; do
  chain_dump="$(dump_section "$session_out" "$chain_name")"
  # No `grep -q` in a pipeline: under `set -o pipefail` the SIGPIPE exit status
  # would be reported instead of the match result.
  if [[ -n "$(dump_section "$session_out" "@missing:$chain_name")" ]]; then
    die "egress chain $chain_name did not survive (refusing unfiltered start) \
(namespace read: $session_netns; podman rootless netns keeper: $(keeper_state))" 2
  fi
  verify_dump "$chain_name" "$chain_dump" "$session_netns"
done
printf 'worker-egress-guard: filter live (%s, %s + %s, subnet %s, namespace %s)\n' \
  "$mode" "$chain" "$input_chain" "$subnet" "$session_netns"
