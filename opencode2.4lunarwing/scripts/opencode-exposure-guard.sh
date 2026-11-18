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
#   opencode auth login <http(s) target>
#                             registers a "well-known" remote-config origin
#                             in the worker's own state (WI#750, upstream
#                             WellKnown.add). Its config document outranks
#                             the user and project layers, and its
#                             integration.use resource is the origin URL,
#                             which the managed `opencode*` deny does not
#                             match. Only an http:/https: target registers
#                             an origin, so `auth list`, `auth logout` and
#                             `auth login <integration-id|name>` still pass.
#
# `auth` and `login` may be separated by flags (with or without a separate
# value), so the scan does not assume `login` follows `auth` immediately: it
# stays in the "looking for login" state across any token that is not
# `login`, and only a literal `login` token turns the target scan on. A
# one-token lookahead would be desynced by `auth --log-level DEBUG login
# <url>`. Once `login` is seen, every remaining token is checked for an
# http:/https: scheme (case-insensitively): the target is positional, so it
# cannot hide behind a flag. The check normalises each token the way
# upstream's URL parser does before it matches the scheme — leading and
# trailing C0 controls and space trimmed, tab, line feed and carriage return
# dropped anywhere — so a leading space, tab or other control character, a
# tab, LF or CR inside the scheme, and a trailing space are all refused too,
# not only a token that starts with the bare scheme (WI#754). The scan errs
# toward refusing: a `run` prompt whose unquoted tokens happen to read
# `auth login <url>` is refused too.
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
# first, then from `experimental.<key>` in the highest-priority config
# document that sets `experimental`, which can be a project's own
# opencode.json in the task-writable workspace, then from the channel
# default. A leading `-` turns a flag off, so this value keeps the flag off
# whatever the caller's environment or a workspace file says. It replaces the
# inherited value instead of extending it: the lane runs with no flag
# opt-ins, so OPENCODE_FLAGS=all must not survive. An operator's own
# `-<key>` entries are dropped with it; the wrapper owns this variable.
#
# What this does not cover: the pin gates the event watches only, not what a
# worker with a Console credential also takes from the Console with the flag
# off — Console-authored instructions, skills, providers, remote MCP servers,
# policies and the web-search selection (WI#665). Children the real binary
# spawns inherit the value, except a managed service started from a
# service.json that carries its own `env` map; the refusal of
# `opencode service` above and the entrypoint's boot-time reset of that file
# are the controls for that path. The builder stage fails the image when the
# reviewed text behind this pin stops holding
# (scripts/compile_out_remote_tunnel.py) — a text tripwire, not a proof.
#
# exposure_policy_assert.py requires the two lines at the end of this file to
# stay exactly as they are, unindented and last; REAL_OPENCODE (apart from its
# own assignment), exec and OPENCODE_FLAGS in no other code line; and the
# whole code body to match its reviewed digest.
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

# Upstream decides whether a target registers a well-known origin with a URL
# parser (URL.canParse plus an http:/https: protocol test), and that parser
# trims leading and trailing C0 controls and space and drops tab, line feed
# and carriage return anywhere in its input. Reproduce that normalisation
# first, so ` https://…`, `\thttps://…`, `\001https://…`, `ht\ttps://…` and a
# trailing space cannot pass this wrapper while upstream still reads an
# http(s) URL (WI#754).
normalise_target() {
  printf '%s' "$1" | tr -d '\011\012\015' |
    sed -e 's/^[[:cntrl:] ]*//' -e 's/[[:cntrl:] ]*$//'
}

is_http_url() {
  case "$(normalise_target "$1")" in
    [Hh][Tt][Tt][Pp]:* | [Hh][Tt][Tt][Pp][Ss]:*) return 0 ;;
    *) return 1 ;;
  esac
}

want_host=0
want_login=0
in_login=0
for arg in "$@"; do
  if [ "$want_host" = 1 ]; then
    want_host=0
    is_loopback "$arg" || refuse "--hostname $arg" "the worker's opencode server must bind loopback only"
    continue
  fi
  if [ "$in_login" = 1 ]; then
    is_http_url "$arg" && refuse "auth login $arg" "well-known remote-config origins are disabled in LunarWing worker images"
  elif [ "$want_login" = 1 ]; then
    case "$arg" in
      login) want_login=0; in_login=1; continue ;;
    esac
  fi
  case "$arg" in
    service) refuse "service" "explicit service commands are disabled by the worker wrapper" ;;
    pair) refuse "pair" "pairing links are disabled in LunarWing worker images" ;;
    upgrade | update) refuse "$arg" "self-upgrade would install an unpinned upstream build outside the compile-out guard" ;;
    --service | --service=*) refuse "serve --service" "explicit service commands are disabled by the worker wrapper" ;;
    --hostname) want_host=1 ;;
    --hostname=*) is_loopback "${arg#--hostname=}" || refuse "--hostname ${arg#--hostname=}" "the worker's opencode server must bind loopback only" ;;
    auth) want_login=1 ;;
  esac
done
[ "$want_host" = 0 ] || refuse "--hostname" "missing value — the worker's opencode server must bind loopback only"

export OPENCODE_FLAGS="-console_watches"
exec "$REAL_OPENCODE" "$@"
