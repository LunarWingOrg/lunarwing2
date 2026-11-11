#!/usr/bin/env bash
# LunarDepth lane configuration, image build, lifecycle, and smoke integration.

LUNARDEPTH_OVERLAY_BASE="${LUNARDEPTH_OVERLAY_BASE:-/var/lib/lunarwing/worker-overlays}"

_lunardepth_renderer() {
  printf '%s' "$LUNARWING_ROOT/lunardepth4lunarwing/scripts/render_overlay.py"
}

_lunardepth_route() {
  local name="$1" env_dir
  env_dir="$(tenant_env_dir "$name")"
  LUNARDEPTH_BASE_URL_RESOLVED="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_BASE_URL)"
  LUNARDEPTH_MODEL_RESOLVED="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_MODEL)"
  LUNARDEPTH_ALLOW_INSECURE_RESOLVED="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_ALLOW_INSECURE_HTTP)"
  LUNARDEPTH_ALLOW_PLACEHOLDER_RESOLVED="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_ALLOW_PLACEHOLDER_KEY)"
  LUNARDEPTH_ALLOW_INSECURE_RESOLVED="${LUNARDEPTH_ALLOW_INSECURE_RESOLVED:-false}"
  LUNARDEPTH_ALLOW_PLACEHOLDER_RESOLVED="${LUNARDEPTH_ALLOW_PLACEHOLDER_RESOLVED:-false}"
  [[ "$LUNARDEPTH_ALLOW_INSECURE_RESOLVED" == "true" || "$LUNARDEPTH_ALLOW_INSECURE_RESOLVED" == "false" ]] \
    || die "LUNARDEPTH_ALLOW_INSECURE_HTTP must be exactly true or false"
  [[ "$LUNARDEPTH_ALLOW_PLACEHOLDER_RESOLVED" == "true" || "$LUNARDEPTH_ALLOW_PLACEHOLDER_RESOLVED" == "false" ]] \
    || die "LUNARDEPTH_ALLOW_PLACEHOLDER_KEY must be exactly true or false"
  [[ -n "$LUNARDEPTH_BASE_URL_RESOLVED" && -n "$LUNARDEPTH_MODEL_RESOLVED" ]] \
    || die "LunarDepth requires explicit LUNARDEPTH_BASE_URL and LUNARDEPTH_MODEL; run configure-lunardepth $name first"
  python3 - "$(_lunardepth_renderer)" "$LUNARDEPTH_BASE_URL_RESOLVED" "$LUNARDEPTH_MODEL_RESOLVED" "$LUNARDEPTH_ALLOW_INSECURE_RESOLVED" <<'PY'
import importlib.util
import sys

path, base_url, model, allow = sys.argv[1:]
spec = importlib.util.spec_from_file_location("lunardepth_overlay", path)
if spec is None or spec.loader is None:
    raise SystemExit(1)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.validate_route(base_url, model, allow == "true")
PY
}

lunardepth_overlay_path() {
  printf '%s' "$LUNARDEPTH_OVERLAY_BASE/$1/lunarwing-lane.patch.yml"
}

render_lunardepth_overlay() {
  local name="$1" base tenant_dir output
  _lunardepth_route "$name"
  base="$LUNARDEPTH_OVERLAY_BASE"
  [[ ! -L "$base" ]] || die "LunarDepth overlay root must not be a symlink"
  install -d -o root -g root -m 0755 "$base"
  tenant_dir="$base/$name"
  [[ ! -L "$tenant_dir" ]] || die "LunarDepth overlay tenant path must not be a symlink"
  install -d -o root -g root -m 0555 "$tenant_dir"
  output="$(lunardepth_overlay_path "$name")"
  local -a args=(--template "$LUNARWING_ROOT/lunardepth4lunarwing/config/lunarwing-lane.patch.yml"
    --output "$output" --base-url "$LUNARDEPTH_BASE_URL_RESOLVED" --model "$LUNARDEPTH_MODEL_RESOLVED")
  [[ "$LUNARDEPTH_ALLOW_INSECURE_RESOLVED" != "true" ]] || args+=(--allow-insecure-http)
  python3 "$(_lunardepth_renderer)" "${args[@]}" || die "could not render the LunarDepth effective overlay"
  [[ -f "$output" && ! -L "$output" && "$(stat -c '%u:%a' "$output")" == "0:444" ]] \
    || die "LunarDepth overlay ownership/mode verification failed"
  LUNARDEPTH_OVERLAY_FILE="$output"
}

configure_lunardepth() {
  local name="$1" model="${2:-}" base_url="${3:-}" api_key="${4:-}"
  local allow_insecure="${5:-preserve}" allow_placeholder="${6:-preserve}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ports_migrate
  [[ -n "$model" || -n "$base_url" || -n "$api_key" || "$allow_insecure" != "preserve" || "$allow_placeholder" != "preserve" ]] \
    || die "configure-lunardepth: pass --model, --base-url, --api-key-file, or an explicit exception option"
  local env_dir existing_key
  env_dir="$(tenant_env_dir "$name")"
  [[ -n "$base_url" ]] || base_url="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_BASE_URL)"
  [[ -n "$model" ]] || model="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_MODEL)"
  existing_key="$(read_tenant_env_value "$name" "$env_dir/lunardepth-task.env" LUNARDEPTH_API_KEY)"
  [[ -n "$api_key" ]] || api_key="$existing_key"
  [[ "$allow_insecure" != "preserve" ]] || allow_insecure="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_ALLOW_INSECURE_HTTP)"
  [[ "$allow_placeholder" != "preserve" ]] || allow_placeholder="$(read_tenant_env_value "$name" "$env_dir/lunardepth.env" LUNARDEPTH_ALLOW_PLACEHOLDER_KEY)"
  allow_insecure="${allow_insecure:-false}"
  allow_placeholder="${allow_placeholder:-false}"
  [[ -n "$base_url" && -n "$model" ]] || die "configure-lunardepth: explicit base URL and model are required"
  if [[ "$allow_placeholder" != "true" && -z "$api_key" ]]; then
    die "configure-lunardepth: --api-key-file is required unless --allow-unauthenticated-gateway is set"
  fi
  if [[ "$allow_insecure" != "true" && "$allow_insecure" != "false" ]] \
      || [[ "$allow_placeholder" != "true" && "$allow_placeholder" != "false" ]]; then
    die "configure-lunardepth: exception options must be exactly true or false"
  fi
  LUNARDEPTH_BASE_URL_RESOLVED="$base_url" LUNARDEPTH_MODEL_RESOLVED="$model" \
    LUNARDEPTH_ALLOW_INSECURE_RESOLVED="$allow_insecure" LUNARDEPTH_ALLOW_PLACEHOLDER_RESOLVED="$allow_placeholder"
  python3 - "$(_lunardepth_renderer)" "$base_url" "$model" "$allow_insecure" <<'PY'
import importlib.util
import sys

path, base_url, model, allow = sys.argv[1:]
spec = importlib.util.spec_from_file_location("lunardepth_overlay", path)
if spec is None or spec.loader is None:
    raise SystemExit(1)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.validate_route(base_url, model, allow == "true")
PY

  local public_staged task_staged
  public_staged="$(mktemp)" || return 1
  task_staged="$(mktemp)" || { rm -f "$public_staged"; return 1; }
  {
    printf 'LUNARDEPTH_BASE_URL=%s\n' "$base_url"
    printf 'LUNARDEPTH_MODEL=%s\n' "$model"
    printf 'LUNARDEPTH_ALLOW_INSECURE_HTTP=%s\n' "$allow_insecure"
    printf 'LUNARDEPTH_ALLOW_PLACEHOLDER_KEY=%s\n' "$allow_placeholder"
  } >"$public_staged"
  {
    printf 'LUNARDEPTH_BASE_URL=%s\n' "$base_url"
    printf 'LUNARDEPTH_MODEL=%s\n' "$model"
    printf 'LUNARDEPTH_ALLOW_INSECURE_HTTP=%s\n' "$allow_insecure"
    printf 'LUNARDEPTH_ALLOW_PLACEHOLDER_KEY=%s\n' "$allow_placeholder"
    [[ "$allow_placeholder" == "true" || -z "$api_key" ]] || printf 'LUNARDEPTH_API_KEY=%s\n' "$api_key"
  } >"$task_staged"
  if ! write_tenant_file_atomic "$name" "$env_dir/lunardepth.env" 600 owned-parent <"$public_staged" \
      || ! write_tenant_file_atomic "$name" "$env_dir/lunardepth-task.env" 600 owned-parent <"$task_staged"; then
    rm -f "$public_staged" "$task_staged"
    die "could not write LunarDepth tenant configuration"
  fi
  rm -f "$public_staged" "$task_staged"
  render_lunardepth_overlay "$name"
  ports_enable_worker "$name" lunardepth
  ensure_external_worker_config "$name" lunardepth lunardepth_wss \
    || die "could not safely write the LunarDepth external-worker config for tenant '$name'"
  say "LunarDepth configured for tenant '$name' (explicit TensorMoon-compatible route; key staged only in the task env)"
}

lunardepth_preflight() {
  local name="$1"
  render_lunardepth_overlay "$name"
  opencode2_builder_supported || die "${OPENCODE2_BUILDER_FLOOR_PROBLEM/opencode2/LunarDepth}"
}

build_lunardepth_worker() {
  local no_cache="${1:-false}" with_toolchains="${2:-false}" source_root="${3:-$LUNARWING_ROOT}" release_ref="${4:-}"
  _prepare_worker_build_source "$source_root" lunardepth4lunarwing "$release_ref"
  source_root="$WORKER_BUILD_SOURCE_ROOT"
  local lane_dir="$source_root/lunardepth4lunarwing" source_revision="$WORKER_BUILD_SOURCE_REVISION"
  [[ -z "$WORKER_BUILD_ARCHIVE_ROOT" ]] || _worker_build_context_begin
  [[ -d "$lane_dir" ]] || die "LunarDepth worker dir not found at $lane_dir"
  opencode2_builder_supported || die "${OPENCODE2_BUILDER_FLOOR_PROBLEM/opencode2/LunarDepth}"
  local -a revision_args=(--build-arg "SOURCE_REVISION=$source_revision")
  local -a cache_args=() toolchain_args=()
  [[ "$no_cache" != "true" ]] || cache_args=(--no-cache)
  [[ "$with_toolchains" != "true" ]] || toolchain_args=(--build-arg WITH_TOOLCHAINS=true)
  local candidate_tag="lunarwing-worker-lunardepth:candidate-$$" iid_file image_id output_tag
  output_tag="$(worker_image_ref lunardepth)"
  iid_file="$(mktemp "${TMPDIR:-/tmp}/lw-lunardepth-iid.XXXXXX")" || die "cannot create LunarDepth image-id file"
  local -a identity_args=(--iidfile "$iid_file")
  if [[ "$CONTAINER_RT" == "podman" ]]; then
    local -a layer_args=()
    [[ "$no_cache" != "true" ]] || layer_args=(--layers=false)
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" "${revision_args[@]}" \
      --network=host --format docker "${identity_args[@]}" -t "$candidate_tag" "$lane_dir" \
      || { rm -f "$iid_file"; die "LunarDepth worker image build failed"; }
  else
    DOCKER_BUILDKIT=1 docker build "${cache_args[@]}" "${toolchain_args[@]}" "${revision_args[@]}" \
      "${identity_args[@]}" -t "$candidate_tag" "$lane_dir" \
      || { rm -f "$iid_file"; die "LunarDepth worker image build failed"; }
  fi
  image_id="$(<"$iid_file")"
  rm -f "$iid_file"
  [[ "$image_id" =~ ^(sha256:)?[0-9a-f]{64}$ ]] || die "LunarDepth build did not produce an immutable image ID"
  if ! _verify_lunardepth_image_out_of_band "$image_id" "$source_root"; then
    "$CONTAINER_RT" rmi "$candidate_tag" >/dev/null 2>&1 || true
    die "out-of-band LunarDepth image verification failed for $image_id"
  fi
  "$CONTAINER_RT" tag "$image_id" "$output_tag" || die "cannot promote the verified LunarDepth image"
  "$CONTAINER_RT" rmi "$candidate_tag" >/dev/null 2>&1 || true
  say "LunarDepth worker image built and verified: $output_tag (revision $source_revision)"
  _worker_build_context_finish
}

_verify_lunardepth_image_out_of_band() (
  local image_ref="$1" source_root="$2" tmp cid archive config node_image _status
  tmp="$(mktemp -d /var/tmp/lw-lunardepth-oob.XXXXXX)" || return 1
  cid=''
  trap '_status=$?; [[ -z "$cid" ]] || "$CONTAINER_RT" rm -f "$cid" >/dev/null 2>&1 || true; rm -rf "$tmp"; exit "$_status"' EXIT
  cid="$("$CONTAINER_RT" create --pull=never "$image_ref" 2>/dev/null)" || cid=''
  [[ -n "$cid" ]] || return 1
  archive="$tmp/rootfs.tar"
  config="$tmp/image-config.json"
  "$CONTAINER_RT" export --output "$archive" "$cid" || return 1
  "$CONTAINER_RT" image inspect "$image_ref" >"$config" || return 1
  install -d -m 0700 "$tmp/scripts"
  cp "$source_root/lunardepth4lunarwing/scripts/package.json" \
    "$source_root/lunardepth4lunarwing/scripts/package-lock.json" \
    "$source_root/lunardepth4lunarwing/scripts/tsconfig.json" \
    "$source_root/lunardepth4lunarwing/scripts/"*.ts "$tmp/scripts/"
  node_image='docker.io/library/node:22.23.4-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da'
  "$CONTAINER_RT" run --rm --pull=never --network=host --cap-drop ALL \
    --security-opt no-new-privileges --entrypoint /bin/bash \
    --volume "$tmp/scripts:/work:rw" "$node_image" \
    -c 'cd /work && npm ci --ignore-scripts && npx tsc -p tsconfig.json' || return 1
  python3 "$source_root/lunardepth4lunarwing/scripts/verify_exposure_out_of_band.py" \
    --archive "$archive" --config "$config" --lane-root "$source_root/lunardepth4lunarwing" \
    --compiled-root "$tmp/scripts"
)

start_tenant_lunardepth() {
  local name="$1" launch_contract="no-ssh-agent;non-root-split-v1;lunardepth-ipc-v2"
  ensure_container_runtime
  local -a userns_args=()
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    launch_contract="$LUNARDEPTH_ROOTLESS_LAUNCH_CONTRACT"
    userns_args=(--userns "$LUNARDEPTH_USERNS")
  fi
  local wss_port container_name
  wss_port="$(ports_get "$name" lunardepth_wss)" || true
  container_name="lunarwing-lunardepth-$name"
  [[ -n "$wss_port" ]] || { say "no lunardepth_wss port allocated for $name (skipping)"; return 0; }
  if ! _ensure_tenant_image "$name" lunarwing-worker-lunardepth:latest; then
    say "LunarDepth worker image not available; run 'build-lunardepth-worker' first (skipping)"
    return 0
  fi
  render_lunardepth_overlay "$name"
  launch_contract="$launch_contract;overlay=$(sha256sum "$LUNARDEPTH_OVERLAY_FILE" | cut -d' ' -f1)"
  launch_contract="$launch_contract;task-env=$(sha256sum "$(tenant_env_dir "$name")/lunardepth-task.env" | cut -d' ' -f1)"
  launch_contract="$(_worker_launch_contract "$name" lunarwing-worker-lunardepth:latest "$launch_contract")"
  prepare_worker_egress "$name" lunardepth
  local image_user
  image_user="$(_ctr "$name" image inspect --format '{{.Config.User}}' lunarwing-worker-lunardepth:latest 2>/dev/null || true)"
  case "$image_user" in
    "$LUNARDEPTH_CONTAINER_UID"|"$LUNARDEPTH_CONTAINER_UID:$LUNARDEPTH_CONTAINER_GID") ;;
    *) warn_err "LunarDepth image has user '$(sanitize_display_value "${image_user:-unset}")', expected $LUNARDEPTH_CONTAINER_UID:$LUNARDEPTH_CONTAINER_GID (WI#454 task default); rebuild it with lunardepth4lunarwing/Dockerfile"; return 3 ;;
  esac

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _start_lunardepth_quadlet "$name" "$launch_contract" "$wss_port"
    return "$?"
  fi
  _start_lunardepth_imperative "$name" "$launch_contract" "$wss_port" "${userns_args[@]}"
}

_start_lunardepth_quadlet() {
  local name="$1" launch_contract="$2" wss_port="$3"
  _wait_user_manager "$name"
  local service="lunarwing-lunardepth-${name}.service" quadlet_file
  quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-lunardepth-${name}.container"
  render_worker_quadlet "$name" lunardepth 8443
  _systemctl_user "$name" daemon-reload 2>/dev/null || true
  reconcile_worker_task_quadlet "$name" lunardepth "$launch_contract" || return 3
  if _container_config_changed "$name" "lunarwing-lunardepth-${name}" "$quadlet_file" "$launch_contract"; then
    _recreate_quadlet_container "$name" "$service" "lunarwing-lunardepth-${name}"
  fi
  _systemctl_user "$name" reset-failed "$service" >/dev/null 2>&1 || true
  if _systemctl_user "$name" start "$service" >/dev/null 2>&1; then
    worker_egress_verify_after_start "$name" "lunarwing-lunardepth-${name}"
    _store_container_hash "$name" "lunarwing-lunardepth-${name}" "$quadlet_file" "$launch_contract"
    say "LunarDepth worker ready via quadlet ($service, WSS port $wss_port)"
  else
    say "WARNING: $service failed to start" >&2
    _systemctl_user "$name" status "$service" --no-pager --full >&2 || true
  fi
}

_start_lunardepth_imperative() {
  local name="$1" launch_contract="$2" wss_port="$3"
  shift 3
  local -a userns_args=("$@")
  prepare_worker_task_launch "$name" lunardepth
  start_worker_task_container "$name" lunardepth lunarwing-worker-lunardepth:latest \
    "$TASK_WORKSPACE_DIR" TASK_USERNS_ARGS TASK_ENV_FLAGS TASK_MOUNTS TASK_RESTART_ARG
  local container_name="lunarwing-lunardepth-$name"
  local -a healthcheck_args=()
  if _openrc_podman_uses_init_health; then
    _reconcile_openrc_podman_healthcheck "$name" "$container_name"
    healthcheck_args=(--no-healthcheck)
  fi
  worker_egress_reconcile_container "$name" "$container_name"
  if _ctr_inspect "$name" "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed "$name" "$container_name" "$env_file_for_hash" "$launch_contract"; then
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr_inspect "$name" -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      _store_container_hash "$name" "$container_name" "$env_file_for_hash" "$launch_contract"
      _register_worker_unit "$name" lunardepth
      return 0
    else
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash "$name" "$container_name" "$env_file_for_hash" "$launch_contract"
      _register_worker_unit "$name" lunardepth
      return 0
    fi
  fi
  if ! _ctr_inspect "$name" "$container_name" &>/dev/null; then
    _create_lunardepth_bridge "$name" "$launch_contract" "$wss_port" "${userns_args[@]}" "${healthcheck_args[@]}"
  fi
  _register_worker_unit "$name" lunardepth
  say "LunarDepth worker ready ($container_name, WSS port $wss_port)"
}

_create_lunardepth_bridge() {
  local name="$1" launch_contract="$2" wss_port="$3"
  shift 3
  local tenant_env_path worker_env_path
  tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"
  worker_env_path="$(tenant_env_dir "$name")/lunardepth.env"
  local -a env_flags=() restart_arg=() health_publish=()
  local agent_token
  agent_token="$(resolve_worker_auth_token "$name" "$tenant_env_path")" || die "could not resolve the scoped worker credential"
  [[ -n "$agent_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$agent_token")
  [[ ! -f "$worker_env_path" ]] || env_flags+=(--env-file "$worker_env_path")
  [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
  local host_health_port
  host_health_port="$(ports_get "$name" lunardepth_health)" || true
  [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
  local workspace_dir="$TASK_WORKSPACE_DIR"
  _ctr "$name" run -d \
    --name "lunarwing-lunardepth-$name" \
    "${userns_args[@]}" \
    --user "$(worker_bridge_uid lunardepth):$LUNARDEPTH_CONTAINER_GID" \
    --security-opt no-new-privileges \
    --cap-drop ALL \
    -e LUNARWING_CONTAINER_ROLE=split-bridge \
    -e LUNARWING_WORKER_ID="worker-lunardepth-${name}" \
    -e WS_PORT="$wss_port" \
    -e HEALTH_PORT=8443 \
    -e WS_BIND_HOST=0.0.0.0 \
    -e WS_PATH=/ws/agent \
    "${env_flags[@]}" \
    "${TASK_WORKSPACE_ARGS[@]}" \
    -p "127.0.0.1:${wss_port}:${wss_port}" \
    "${health_publish[@]}" \
    -v "$workspace_dir:/workspace:z" \
    -v "$(worker_ipc_volume_name lunardepth "$name"):/run/lunarwing-shared:z" \
    "${restart_arg[@]}" \
    "${WORKER_EGRESS_LAUNCH_ARGS[@]}" \
    --label "lunarwing.egress=$(worker_egress_network_name "$name")" \
    --label "lunarwing.worker-role=bridge" \
    lunarwing-worker-lunardepth:latest \
    --mode websocket >/dev/null
  _store_container_hash "$name" "lunarwing-lunardepth-$name" "$tenant_env_path" "$launch_contract"
}

stop_tenant_lunardepth() {
  local name="$1"
  local container_name="lunarwing-lunardepth-$name"
  ensure_container_runtime
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "$OPENRC_INITD_DIR/${container_name}" ]]; then
    _openrc_stop_worker_nondestructive "$container_name"
  elif [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _systemctl_user "$name" stop "${container_name}.service" >/dev/null 2>&1 \
      || _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
  elif _ctr_inspect "$name" "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
  fi
  stop_worker_task_container "$name" lunardepth
}

lunardepth_worker_smoke() {
  local name="$1" task_container
  task_container="$(worker_task_container_name lunardepth "$name")"
  _ctr_inspect "$name" -f '{{.State.Running}}' "$task_container" | grep -q true \
    || die "LunarDepth task container is not running"
  _ctr "$name" exec "$task_container" env SMOKE_ROLE=task SMOKE_RUN_SANDBOX=1 \
    python3 /app/scripts/runtime_smoke.py --profile remote
  _ctr_inspect "$name" -f '{{.State.Running}}' "lunarwing-lunardepth-$name" | grep -q true \
    || die "LunarDepth bridge container is not running"
  _ctr "$name" exec "lunarwing-lunardepth-$name" env SMOKE_ROLE=bridge \
    python3 /app/scripts/runtime_smoke.py --profile remote
}
