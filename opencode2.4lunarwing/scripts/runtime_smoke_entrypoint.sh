#!/usr/bin/env bash
# WI#424 control 6: exercise the real entrypoint during the candidate build.
set -euo pipefail

SMOKE_ROOT="/tmp/opencode2-runtime-smoke-entrypoint"
ENTRYPOINT_PID=""
READY=false

cleanup() {
  local -a children=()
  if [[ -n "$ENTRYPOINT_PID" && -r "/proc/$ENTRYPOINT_PID/task/$ENTRYPOINT_PID/children" ]]; then
    read -r -a children <"/proc/$ENTRYPOINT_PID/task/$ENTRYPOINT_PID/children" || true
  fi
  if [[ -n "$ENTRYPOINT_PID" ]]; then
    kill "${children[@]}" "$ENTRYPOINT_PID" >/dev/null 2>&1 || true
    wait "$ENTRYPOINT_PID" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$SMOKE_ROOT"
}
trap cleanup EXIT
rm -rf -- "$SMOKE_ROOT"

export HOME="$SMOKE_ROOT/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
export TMPDIR="$SMOKE_ROOT/tmp"
export WORKSPACE_ROOT="$SMOKE_ROOT/workspace"
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

/app/entrypoint.sh --mode websocket >"$SMOKE_ROOT/entrypoint.log" 2>&1 &
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

python3 /app/scripts/runtime_smoke.py --profile container --health-port "$HEALTH_PORT"
