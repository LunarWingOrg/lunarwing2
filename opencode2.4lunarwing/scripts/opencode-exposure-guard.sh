#!/bin/sh
# opencode-exposure-guard.sh — WI#357: workers must never self-expose.
#
# Installed as /usr/local/bin/opencode in the worker image; the real pinned
# binary lives at /usr/local/lib/opencode2/opencode. LunarWing owns every
# public exposure path (ic/src/tunnel); the only legitimate instruction
# channel into this worker is the daemon's job dispatch -> the in-container
# bridge -> the opencode API on loopback. This wrapper refuses the upstream
# command surface that could break that rule.
#
# This is a NAIVE-USE guard, not a security boundary: the real binary stays
# executable by the agent uid (the entrypoint's own serve runs through this
# wrapper as that uid), so a deliberately obfuscated invocation (exec'ing
# the real binary path, os.execv, Bun.spawn) passes it. The boundary against
# a compromised agent is the deferred egress allowlist (WI#357 control 1).
#
# Every argv token is scanned (run-79 EI-2/EI-3, run-84 EU-2), because the
# CLI accepts global/root flags before the subcommand (--print-logs,
# --log-level=DEBUG, --auto, …) and serve accepts multiple --hostname
# values:
#
#   opencode service ...      service mode; v2.0.26+ couples it to the
#                             hosted OpenTunnel relay (no self-host option)
#   opencode pair ...         issues pairing links into the agent API
#   opencode serve --service  service mode via the serve flag
#   opencode ... --hostname <non-loopback>
#                             the worker's server must stay on loopback
#                             (every --hostname occurrence is checked)
#
# Everything else execs the real binary with its argv unchanged. Server-backed
# commands can spawn that binary without passing through this wrapper;
# entrypoint.sh resets service.json to disabled=true at boot so they default to
# private servers. The agent can rewrite that live config, so this is still a
# naive-use guard.
#
# One environment variable is forced on every start (WI#609, !2398 AAV-1):
# OPENCODE_FLAGS=-console_watches. v2.2.7 ships Console event watches, a way
# to steer a session from OpenCode Console (a hosted third party), behind the
# feature flag `console_watches`. Upstream resolves a flag from OPENCODE_FLAGS
# first, then from `experimental.<key>` in any config document, a project's
# own opencode.json in the task-writable workspace included, then from the
# channel default. A leading `-` turns a flag off, so this value keeps the
# flag off whatever the caller's environment or a workspace file says. It
# replaces the inherited value instead of extending it: the lane runs with no
# flag opt-ins, so OPENCODE_FLAGS=all must not survive either. Children the
# real binary spawns inherit it.
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

want_host=0
for arg in "$@"; do
  if [ "$want_host" = 1 ]; then
    want_host=0
    is_loopback "$arg" || refuse "--hostname $arg" "the worker's opencode server must bind loopback only"
    continue
  fi
  case "$arg" in
    service) refuse "service" "explicit service commands are disabled by the worker wrapper" ;;
    pair) refuse "pair" "pairing links are disabled in LunarWing worker images" ;;
    upgrade | update) refuse "$arg" "self-upgrade would install an unpinned upstream build outside the compile-out guard" ;;
    --service | --service=*) refuse "serve --service" "explicit service commands are disabled by the worker wrapper" ;;
    --hostname) want_host=1 ;;
    --hostname=*) is_loopback "${arg#--hostname=}" || refuse "--hostname ${arg#--hostname=}" "the worker's opencode server must bind loopback only" ;;
  esac
done
[ "$want_host" = 0 ] || refuse "--hostname" "missing value — the worker's opencode server must bind loopback only"

export OPENCODE_FLAGS="-console_watches"
exec "$REAL_OPENCODE" "$@"
