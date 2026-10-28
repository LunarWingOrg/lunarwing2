#!/usr/bin/env bash
# WI#424 control 6: exercise the real entrypoint during the candidate build.
#
# The smoke runs as the image's own runtime user (10003) — not as root — so it
# observes the permissions, listeners and wrapper behaviour a real worker has
# (run-226 OQ-6). Root only creates/cleans the scratch tree.
set -euo pipefail

SMOKE_ROOT="/tmp/opencode2-runtime-smoke-entrypoint"
SMOKE_UID="${SMOKE_UID:-10003}"
SMOKE_GID="${SMOKE_GID:-10003}"
ENTRYPOINT_PID=""
READY=false

# The build container must be empty when this script returns: buildkit-runc
# removes the container's cgroup right after the RUN, and any process still
# alive in it (opencode serve finishing its shutdown, a child it spawned)
# makes that rmdir fail with "device or resource busy" although every check
# passed (master pipelines 6644 and 6651). Killing the entrypoint plus its
# direct children and waiting for the entrypoint alone did not do it. So:
# terminate every live process of the runtime user, wait for the set to
# drain, escalate to SIGKILL, and only then return.
SMOKE_DRAIN_SECS="${SMOKE_DRAIN_SECS:-10}"

runtime_user_pids() {
  # Live (non-zombie) pids whose real uid is the runtime user, except this shell.
  local status pid key value uid state
  for status in /proc/[0-9]*/status; do
    pid="${status#/proc/}"
    pid="${pid%/status}"
    [[ "$pid" != "$$" ]] || continue
    uid=""
    state=""
    while IFS=$' \t' read -r key value _; do
      case "$key" in
        Uid:) uid="$value" ;;
        State:) state="$value" ;;
      esac
      [[ -z "$uid" || -z "$state" ]] || break
    done <"$status" 2>/dev/null || continue
    [[ "$uid" == "$SMOKE_UID" && "$state" != Z ]] || continue
    printf '%s\n' "$pid"
  done
}

drain_runtime_user() {
  local sig deadline
  local -a pids=()
  for sig in TERM KILL; do
    mapfile -t pids < <(runtime_user_pids)
    (( ${#pids[@]} )) || return 0
    kill "-$sig" "${pids[@]}" >/dev/null 2>&1 || true
    deadline=$((SECONDS + SMOKE_DRAIN_SECS))
    while (( SECONDS < deadline )); do
      mapfile -t pids < <(runtime_user_pids)
      (( ${#pids[@]} )) || return 0
      sleep 0.2
    done
  done
  mapfile -t pids < <(runtime_user_pids)
  (( ${#pids[@]} )) || return 0
  printf 'runtime smoke: %s process(es) of uid %s survived SIGKILL: %s\n' \
    "${#pids[@]}" "$SMOKE_UID" "${pids[*]}" >&2
  return 1
}

cleanup() {
  drain_runtime_user || true
  [[ -z "$ENTRYPOINT_PID" ]] || wait "$ENTRYPOINT_PID" >/dev/null 2>&1 || true
  rm -rf -- "$SMOKE_ROOT"
}
trap cleanup EXIT
rm -rf -- "$SMOKE_ROOT"
mkdir -p "$SMOKE_ROOT"
chown "$SMOKE_UID:$SMOKE_GID" "$SMOKE_ROOT"

# Run a given command as the image runtime user. `setpriv` ships in Debian's
# util-linux, so its absence means the image is not what we pinned: fail loudly
# instead of guessing at a fallback that would silently run as root (run-232
# PB-3 — `su` takes a user name, not a uid, so the old fallback never worked).
as_runtime_user() {
  command -v setpriv >/dev/null 2>&1 \
    || { printf 'runtime_smoke: setpriv is missing; cannot drop to uid %s\n' "$SMOKE_UID" >&2; return 1; }
  setpriv --reuid="$SMOKE_UID" --regid="$SMOKE_GID" --clear-groups -- "$@"
}

export HOME="$SMOKE_ROOT/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
export TMPDIR="$SMOKE_ROOT/tmp"
export WORKSPACE_ROOT="$SMOKE_ROOT/workspace"
export WS_STATE_FILE="$SMOKE_ROOT/ws_state.json"
export PYTHONDONTWRITEBYTECODE=1
export OPENCODE2_BASE_URL="http://127.0.0.1:1"
export OPENCODE_PASSWORD="candidate-runtime-smoke-only"
export OPENCODE_SERVER_PASSWORD="$OPENCODE_PASSWORD"
export AGENT_AUTH_TOKEN=""
export HEALTH_PORT=8443
export WS_PORT=19090
export WS_BIND_HOST=0.0.0.0
export OPENCODE_SERVE_PORT=4096
mkdir -p "$HOME" "$TMPDIR" "$WORKSPACE_ROOT"
chown -R "$SMOKE_UID:$SMOKE_GID" "$SMOKE_ROOT"

as_runtime_user /app/entrypoint.sh --mode websocket >"$SMOKE_ROOT/entrypoint.log" 2>&1 &
ENTRYPOINT_PID=$!

for _ in $(seq 1 60); do
  if curl -sf --max-time 2 -o /dev/null "http://127.0.0.1:$HEALTH_PORT/health" \
      && curl -s --max-time 2 -o /dev/null "http://127.0.0.1:$WS_PORT/"; then
    READY=true
    break
  fi
  if ! kill -0 "$ENTRYPOINT_PID" >/dev/null 2>&1; then
    cat "$SMOKE_ROOT/entrypoint.log" >&2
    exit 1
  fi
  sleep 1
done

if [[ "$READY" != "true" ]]; then
  cat "$SMOKE_ROOT/entrypoint.log" >&2
  printf 'runtime smoke: entrypoint listeners did not become ready\n' >&2
  exit 1
fi

# Refuse to certify a smoke that silently ran as root.
if [[ "$(as_runtime_user id -u)" != "$SMOKE_UID" ]]; then
  printf 'runtime smoke: could not drop to the image runtime user %s\n' "$SMOKE_UID" >&2
  exit 1
fi

as_runtime_user python3 /app/scripts/runtime_smoke.py --profile container --health-port "$HEALTH_PORT"
