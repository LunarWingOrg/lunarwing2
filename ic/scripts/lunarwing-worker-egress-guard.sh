#!/usr/bin/env bash
# WI#424 control 1 — load and verify a worker egress allowlist before a worker
# container starts.
#
# Usage: lunarwing-worker-egress-guard.sh <ruleset> [rootless|rootful] [tenant]
#
# Called from every worker boot path (Quadlet `ExecStartPre`, the OpenRC worker
# init script's start(), and mt-admin's prepare_worker_egress). It exists as a
# separate script so the two init systems cannot drift: both run these exact
# steps, and the fail-closed property is testable outside a unit file.
#
# Why the verification is a SEPARATE invocation: `podman unshare
# --rootless-netns` creates the tenant's netns on demand. If that netns does not
# persist (no stable XDG_RUNTIME_DIR — i.e. no lingering user session), the
# ruleset would be loaded into a namespace that disappears with the command, and
# the worker would then start *unfiltered*. Reading the chain back through a
# fresh entry is the only way to prove the filter is still there. A failure is
# fatal: the caller must not start the worker.
#
# Exit codes: 0 filter live and verified, 1 load failed, 2 verification failed,
# 3 usage/environment error.
set -euo pipefail

TABLE="lunarwing_worker_egress"
CHAIN="workers"

die() { printf 'worker-egress-guard: %s\n' "$*" >&2; exit "${2:-3}"; }

[[ $# -ge 1 ]] || die "usage: $0 <ruleset> [rootless|rootful] [tenant]"
ruleset="$1"
mode="${2:-rootless}"
tenant="${3:-}"

case "$mode" in
  rootless|rootful) ;;
  *) die "invalid mode '$mode' (expected rootless or rootful)" ;;
esac

[[ -f "$ruleset" && ! -L "$ruleset" ]] \
  || die "egress ruleset not found or not a regular file: $ruleset"

# `command -v` under `set -e`; an absent nft is handled explicitly below.
NFT="$(command -v nft 2>/dev/null)" || NFT=""
[[ -n "$NFT" ]] || die "nft not found; refusing to start a worker without an egress filter"

PODMAN="$(command -v podman || printf '/usr/bin/podman')"

# Run nft in the namespace that owns the bridge.
#
# Rootless podman keeps the bridge in the TENANT's netns, so the command must
# run as that user. A Quadlet ExecStartPre already runs as the tenant (no third
# argument needed); the OpenRC path runs as root and passes the tenant name, so
# root drops to the tenant here rather than inventing a second code path.
nft_in_netns() {
  if [[ "$mode" != "rootless" ]]; then
    "$NFT" "$@"
    return
  fi
  if [[ -n "$tenant" && "$(id -u)" == "0" ]]; then
    [[ "$tenant" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
      || die "invalid tenant name '$tenant'"
    local uid home
    uid="$(id -u -- "$tenant")" || die "cannot resolve uid for tenant '$tenant'"
    home="$(getent passwd "$tenant" | cut -d: -f6)"
    ( cd / && exec sudo -u "$tenant" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
        "$PODMAN" unshare --rootless-netns "$NFT" "$@" )
    return
  fi
  "$PODMAN" unshare --rootless-netns "$NFT" "$@"
}

nft_in_netns -f "$ruleset" >/dev/null 2>&1 \
  || die "failed to load the egress ruleset $ruleset" 1

dump="$(nft_in_netns list chain inet "$TABLE" "$CHAIN" 2>/dev/null)" \
  || die "egress filter did not survive the load (refusing to run the worker unfiltered)" 2

# The reject rule is the control's whole point; a chain that loaded without it
# would allow everything not explicitly listed.
grep -q 'reject' <<<"$dump" \
  || die "egress chain loaded without its reject rule (refusing to run the worker unfiltered)" 2

# awk rather than `grep -c` so a zero-match count cannot look like a failure.
printf 'worker-egress-guard: filter live (%s, %s rule line(s))\n' \
  "$mode" "$(awk '/^[[:space:]]/ { n += 1 } END { print n + 0 }' <<<"$dump")"
