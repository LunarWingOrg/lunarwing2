#!/bin/bash
set -euo pipefail

[[ "$(id -u)" == "0" && "${LUNARDEPTH_BUILD_SMOKE:-}" == "1" ]] || {
  printf '%s\n' 'runtime_smoke_entrypoint.sh is restricted to the root image-build assertion' >&2
  exit 1
}

TASK_PID=
BRIDGE_PID=
MODEL_PID=
cleanup() {
  status=$?
  trap - EXIT INT TERM
  [[ -z "$TASK_PID" ]] || kill "$TASK_PID" 2>/dev/null || true
  [[ -z "$BRIDGE_PID" ]] || kill "$BRIDGE_PID" 2>/dev/null || true
  [[ -z "$MODEL_PID" ]] || kill "$MODEL_PID" 2>/dev/null || true
  [[ -z "$TASK_PID" ]] || wait "$TASK_PID" 2>/dev/null || true
  [[ -z "$BRIDGE_PID" ]] || wait "$BRIDGE_PID" 2>/dev/null || true
  [[ -z "$MODEL_PID" ]] || wait "$MODEL_PID" 2>/dev/null || true
  rm -f /run/lunardepth-config/lunarwing-lane.patch.yml \
    /home/lunardepth-bridge/.local/state/lunarwing/agent_auth_token \
    /run/lunardepth-state/local-auth-token
  exit "$status"
}
trap cleanup EXIT INT TERM

/usr/bin/python3 /app/scripts/render_overlay.py \
  --template /app/config/lunarwing-lane.patch.yml \
  --output /run/lunardepth-config/lunarwing-lane.patch.yml \
  --base-url http://127.0.0.1:18080/openai/v1 \
  --model tensormoon::function_name::lunarwing

/usr/bin/python3 /app/scripts/runtime_smoke.py --profile fake-model --port 18080 &
MODEL_PID=$!

setpriv --reuid=10005 --regid=10005 --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs \
  env -i PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  HOME=/home/lunardepth LUNARWING_CONTAINER_ROLE=split-task WORKSPACE_ROOT=/workspace \
  LUNARDEPTH_BASE_URL=http://127.0.0.1:18080/openai/v1 \
  LUNARDEPTH_MODEL=tensormoon::function_name::lunarwing \
  LUNARDEPTH_API_KEY=lunardepth-build-smoke-only-not-a-secret \
  LUNARDEPTH_EFFECTIVE_OVERLAY_SHA256="$(sha256sum /run/lunardepth-config/lunarwing-lane.patch.yml | cut -d' ' -f1)" \
  LUNARDEPTH_IMAGE_REVISION=da06c33b2091546dc38c61b95452ce27a4ce32d5 \
  /app/entrypoint.sh &
TASK_PID=$!

setpriv --reuid=10105 --regid=10005 --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs \
  env -i PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  HOME=/home/lunardepth-bridge LUNARWING_CONTAINER_ROLE=split-bridge WORKSPACE_ROOT=/workspace \
  AGENT_AUTH_TOKEN=lunardepth-build-smoke-hub-token-0123456789abcdef \
  WS_BIND_HOST=0.0.0.0 WS_PORT=9090 HEALTH_PORT=8443 \
  LUNARDEPTH_IMAGE_REVISION=da06c33b2091546dc38c61b95452ce27a4ce32d5 \
  /app/entrypoint.sh &
BRIDGE_PID=$!

attempt=0
until curl -fsS --max-time 2 http://127.0.0.1:8443/ready >/dev/null 2>&1; do
  attempt=$((attempt + 1))
  [[ "$attempt" -lt 180 ]] || { printf '%s\n' 'split worker did not become ready' >&2; exit 1; }
  kill -0 "$TASK_PID" 2>/dev/null || { printf '%s\n' 'task role exited during smoke' >&2; exit 1; }
  kill -0 "$BRIDGE_PID" 2>/dev/null || { printf '%s\n' 'bridge role exited during smoke' >&2; exit 1; }
  sleep 1
done

export SMOKE_HUB_TOKEN=lunardepth-build-smoke-hub-token-0123456789abcdef
SMOKE_LOCAL_TOKEN="$(</run/lunardepth-state/local-auth-token)"
export SMOKE_LOCAL_TOKEN
export SMOKE_LISTENER_ROLE=combined
/usr/bin/python3 /app/scripts/runtime_smoke.py --profile remote
/usr/bin/python3 /app/scripts/exposure_policy_assert.py --image-filesystem /
printf '%s\n' 'LunarDepth split-role image smoke passed'
