#!/bin/sh
# opencode-exposure-guard.sh — WI#357: workers must never self-expose.
#
# Installed as /usr/local/bin/opencode in the worker image; the real pinned
# binary lives at /usr/local/lib/opencode2/opencode. LunarWing owns every
# public exposure path (ic/src/tunnel); the only legitimate instruction
# channel into this worker is the daemon's job dispatch -> the in-container
# bridge -> the opencode API on loopback. This wrapper refuses the upstream
# command surface that could break that rule, even when invoked through
# layers the permission ruleset cannot see (bash -c, absolute paths):
#
#   opencode service ...   service mode; v2.0.26+ couples it to the hosted
#                          OpenTunnel relay (no self-host option)
#   opencode pair ...      issues pairing links into the agent API
#   opencode serve --hostname <non-loopback>
#                          the worker's server must stay on loopback
#
# Everything else execs the real binary unchanged.
set -eu

REAL_OPENCODE="${LUNARWING_REAL_OPENCODE:-/usr/local/lib/opencode2/opencode}"

refuse() {
  echo "opencode-exposure-guard: refused 'opencode $1': $2" >&2
  echo "Workers must never self-expose (WI#357); public paths go through the LunarWing daemon's tunnel." >&2
  exit 77
}

is_loopback() {
  case "$1" in
    127.0.0.1 | ::1 | localhost | "[::1]") return 0 ;;
    *) return 1 ;;
  esac
}

case "${1:-}" in
  service)
    refuse "service" "service mode is disabled in LunarWing worker images"
    ;;
  pair)
    refuse "pair" "pairing links are disabled in LunarWing worker images"
    ;;
  serve)
    # Parse args after the subcommand for an explicit --hostname; refuse
    # non-loopback binds. No --hostname means upstream's loopback default.
    hostname=""
    want_next=0
    first=1
    for arg in "$@"; do
      if [ "$first" = 1 ]; then
        first=0
        continue
      fi
      if [ "$want_next" = 1 ]; then
        hostname="$arg"
        break
      fi
      case "$arg" in
        --hostname) want_next=1 ;;
        --hostname=*) hostname="${arg#--hostname=}"; break ;;
      esac
    done
    if [ -n "$hostname" ] && ! is_loopback "$hostname"; then
      refuse "serve --hostname $hostname" "the worker's opencode server must bind loopback only"
    fi
    ;;
esac

exec "$REAL_OPENCODE" "$@"
