#!/bin/sh
# pebble-self-update-guard.sh — WI#425 (pebble lane hardening, control 11
# residual): `pebble self-update` is refused in LunarWing worker images.
#
# Installed as /usr/local/bin/pebble (the real pinned binary moves to
# /usr/local/lib/pebble/pebble). Upstream's self-update fetches GitHub
# releases and replaces the running binary; in a LunarWing worker the
# pebble version is a reviewed, reproducibly-built pin (PEBBLE_REF + the
# reviewed Cargo.lock override), and an in-container self-update would
# replace it with an unreviewed upstream build outside that contract —
# and, per WI#400/WI#422, on the rootful tenant-uid lane the task uid can
# reach the binary. The egress allowlist (WI#424, deferred) will also block
# its GitHub fetch; this wrapper is the naive-use refusal now.
#
# This is a NAIVE-USE guard, not a boundary: task code can still exec the
# real binary path directly. The boundary against a compromised agent is
# the egress allowlist (control 1).
#
# Every argv token is scanned (opencode-lane precedent): the self-update
# subcommand is refused wherever it appears; everything else execs the
# real binary unchanged.
set -eu

REAL_PEBBLE="${LUNARWING_REAL_PEBBLE:-/usr/local/lib/pebble/pebble}"

refuse() {
  echo "pebble-self-update-guard: refused 'pebble $1': $2" >&2
  echo "Workers never self-update; the pebble pin is a reviewed reproducible build (WI#425)." >&2
  exit 77
}

for arg in "$@"; do
  case "$arg" in
    self-update) refuse "self-update" "self-update would replace the pinned, reproducibly built pebble binary with an unreviewed upstream release" ;;
  esac
done

exec "$REAL_PEBBLE" "$@"
