# 02-containers.sh
# Container execution wrapper and tenant worker container lifecycle
# Extracted from lunarwing-mt-admin.sh (Sunburst refactor)
#
configure_opencode() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" ]] \
    || die "configure-opencode: pass --model <model> and/or --base-url <url>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  local tmp
  tmp="$(mktemp)"
  grep -v -e '^OPENCODE_MODEL=' -e '^OPENCODE_BASE_URL=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'OPENCODE_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'OPENCODE_BASE_URL=%s\n' "$base_url" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "opencode LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-opencode-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the opencode worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

start_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  local opencode_launch_contract=""
  local -a opencode_userns_args=()
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    opencode_launch_contract="$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"
    opencode_userns_args=(--userns "$OPENCODE_USERNS")
  fi

  local wss_port container_name opencode_dir
  # `|| true`: ports_get returns nonzero for a genuinely unallocated key; under
  # `set -euo pipefail` a bare assignment would abort the whole start-tenant run
  # before the `[[ -z "$wss_port" ]]` skip below (mirrors start_tenant_vision).
  wss_port="$(ports_get "$name" opencode_wss)" || true
  container_name="lunarwing-opencode-$name"
  opencode_dir="${LUNARWING_ROOT}/opencode4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no opencode_wss port allocated for $name (skipping opencode worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-opencode:latest; then
    say "opencode worker image not available; run 'build-opencode-worker' first (skipping)"
    return 0
  fi
  local image_user
  image_user="$(_ctr "$name" image inspect --format '{{.Config.User}}' \
    lunarwing-worker-opencode:latest 2>/dev/null || true)"
  case "$image_user" in
    "$OPENCODE_CONTAINER_UID"|"$OPENCODE_CONTAINER_UID:$OPENCODE_CONTAINER_GID") ;;
    *)
      die "opencode worker image has user '${image_user:-unset}', expected $OPENCODE_CONTAINER_UID:$OPENCODE_CONTAINER_GID; rebuild it"
      ;;
  esac

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local opencode_service="lunarwing-opencode-${name}.service"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-opencode-${name}.container"
    render_worker_quadlet "$name" opencode 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-opencode-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "$opencode_service" "lunarwing-opencode-${name}"
    fi
    # A bad pre-upgrade image can exhaust StartLimitBurst. Loading a fixed image
    # does not clear that systemd lockout, so reset it before the explicit retry.
    _systemctl_user "$name" reset-failed "$opencode_service" >/dev/null 2>&1 || true
    if _systemctl_user "$name" start "$opencode_service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-opencode-${name}" "$quadlet_file"
      say "opencode worker ready via quadlet ($opencode_service, WSS port $wss_port)"
    else
      say "WARNING: $opencode_service failed to start" >&2
      _systemctl_user "$name" status "$opencode_service" --no-pager --full >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "opencode worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"
      _register_worker_unit "$name" opencode
      return 0
    else
      say "starting existing opencode worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"
      _register_worker_unit "$name" opencode
      say "opencode worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating opencode worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local opencode_env_path
    opencode_env_path="$(tenant_env_dir "$name")/opencode.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local gateway_token
      gateway_token="$(_read_env_value "$tenant_env_path" GATEWAY_AUTH_TOKEN)"
      [[ -n "$gateway_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$gateway_token")

      local llm_api_key
      llm_api_key="$(_read_env_value "$tenant_env_path" LLM_API_KEY)"
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORZERO_API_KEY=$llm_api_key")

      local opencode_model opencode_base_url
      opencode_model="$(_read_env_value "$tenant_env_path" OPENCODE_MODEL)"
      [[ -n "$opencode_model" ]] && env_flags+=(-e "OPENCODE_MODEL=$opencode_model")
      opencode_base_url="$(_read_env_value "$tenant_env_path" OPENCODE_BASE_URL)"
      [[ -n "$opencode_base_url" ]] && env_flags+=(-e "OPENCODE_BASE_URL=$opencode_base_url")
    fi

    if [[ -f "$opencode_env_path" ]]; then
      env_flags+=(--env-file "$opencode_env_path")
    fi

    local study_notes_home study_notes_state_home study_notes_allow_write
    local -a study_notes_mount=()
    study_notes_home="$(tenant_study_notes_home "$name")"
    if [[ -n "$study_notes_home" ]]; then
      validate_study_notes_home "$study_notes_home"
      study_notes_mount=(
        -v "${study_notes_home}:${STUDY_NOTES_CONTAINER_HOME}:ro,z"
        -e "LW2STUDYMCP_HOME=${STUDY_NOTES_CONTAINER_HOME}"
      )
      study_notes_state_home="$(tenant_study_notes_state_home "$name")"
      study_notes_allow_write="$(tenant_study_notes_allow_write "$name")"
      if study_notes_write_enabled "$study_notes_allow_write"; then
        [[ -n "$study_notes_state_home" ]] \
          || die "Study Notes write mode requires LW2STUDYMCP_STATE_HOME"
      fi
      if [[ -n "$study_notes_state_home" ]]; then
        validate_study_notes_state_home "$study_notes_state_home"
        study_notes_mount+=(
          -v "${study_notes_state_home}:${STUDY_NOTES_CONTAINER_STATE_HOME}:ro,z"
          -e "LW2STUDYMCP_STATE_HOME=${STUDY_NOTES_CONTAINER_STATE_HOME}"
        )
      fi
    fi

    local workspace_dir
    workspace_dir="$(prepare_worker_workspace "$name" opencode)"

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" opencode_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    local ssh_agent_socket
    ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    local -a ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      "${opencode_userns_args[@]}" \
      -e LUNARWING_WORKER_ID="worker-opencode-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e OPENCODE_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      "${study_notes_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-opencode:latest \
      --mode websocket >/dev/null
    _store_container_hash \
      "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env" \
      "$opencode_launch_contract"
  fi

  _register_worker_unit "$name" opencode
  say "opencode worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-opencode-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  fi
}

start_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local wss_port container_name
  wss_port="$(ports_get "$name" pebble_wss)"
  container_name="lunarwing-pebble-$name"

  if [[ -z "$wss_port" ]]; then
    say "no pebble_wss port allocated for $name (skipping pebble worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-pebble:latest; then
    say "pebble worker image not available; run 'build-pebble-worker' first (skipping)"
    return 0
  fi

  # systemd + rootless podman: Quadlet .container owns the lifecycle (the unit's
  # [Container] spec creates+runs it), so skip the imperative `_ctr run` below.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-pebble-${name}.container"
    render_worker_quadlet "$name" pebble 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-pebble-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-pebble-${name}.service" "lunarwing-pebble-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-pebble-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-pebble-${name}" "$quadlet_file"
      say "pebble worker ready via quadlet (lunarwing-pebble-${name}.service, WSS port $wss_port)"
    else
      say "WARNING: lunarwing-pebble-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-pebble-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed "$name" "$container_name" "$env_file_for_hash"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "pebble worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" pebble
      return 0
    else
      say "starting existing pebble worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" pebble
      say "pebble worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating pebble worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local pebble_env_path
    pebble_env_path="$(tenant_env_dir "$name")/pebble.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local gateway_token
      gateway_token="$(_read_env_value "$tenant_env_path" GATEWAY_AUTH_TOKEN)"
      [[ -n "$gateway_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$gateway_token")
    fi

    if [[ -f "$pebble_env_path" ]]; then
      env_flags+=(--env-file "$pebble_env_path")
    fi

    local workspace_dir
    workspace_dir="$(tenant_lw_root "$name")/pebble-workspace"
    mkdir -p "$workspace_dir"
    chown "$name:$name" "$workspace_dir"
    chmod 777 "$workspace_dir"

    # HEALTH_PORT=8443 matches the image's baked HEALTHCHECK (in-container).
    # v8: also publish the tenant's dedicated pebble_health port -> container
    # 8443, so the host self-heal pipeline can probe /health directly.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" pebble_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    # SSH agent socket (always included — daemon creates it at startup).
    # :z label for SELinux (Enforcing on Fedora).
    local -a ssh_mount=()
    local ssh_agent_socket
    ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      -e LUNARWING_WORKER_ID="worker-pebble-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e PEBBLE_MODE=websocket \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-pebble:latest >/dev/null
    _store_container_hash "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env"
  fi

  _register_worker_unit "$name" pebble
  say "pebble worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pebble-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  fi
}

build_vision_sidecar_image() {
  ensure_container_runtime
  local sidecar_dir="$SOURCE_REPO/projects/ocr-sidecar"
  [[ -d "$sidecar_dir" ]] || { say "WARNING: $sidecar_dir not found; skipping vision sidecar build"; return 0; }
  if [[ -f "$sidecar_dir/Dockerfile" ]]; then
    "$CONTAINER_RT" build --network=host --format docker -t "$VISION_SIDECAR_IMAGE" -f "$sidecar_dir/Dockerfile" "$sidecar_dir" >/dev/null \
      || { say "WARNING: vision sidecar image build failed (run 'build-vision-sidecar' to retry)"; return 1; }
    say "vision sidecar image built: $VISION_SIDECAR_IMAGE"
  else
    say "WARNING: $sidecar_dir/Dockerfile missing; cannot build vision sidecar"
    return 1
  fi
}

write_tenant_vision_env() {
  local name="$1"
  local env_dir env_path
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/vision.env"
  local token
  token="$(_env_existing "$env_path" LUNARWING_AUTH_TOKEN)"
  token="${token:-$(generate_token)}"
  # Resolve VL backend URL: explicit override wins, else fleet default.
  # Empty (LUNARWING_MT_VL_URL= and DEFAULT_VL_URL unset) = no VL line written;
  # sidecar comes up with VL disabled. Idempotent — vision.env is fully rewritten.
  local vl_url
  vl_url="${LUNARWING_MT_VL_URL:-$DEFAULT_VL_URL}"
  mkdir -p "$env_dir"
  (
    umask 077
    if [[ -n "$vl_url" ]]; then
      cat >"$env_path" <<ENVEOF
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
VL_URL=$vl_url
VL_MODEL=qwen3-vl
ENVEOF
    else
      cat >"$env_path" <<ENVEOF
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
ENVEOF
    fi
  )
  chown "$name:$name" "$env_path"
  printf '%s' "$token"
}

start_tenant_vision() {
  local name="$1"
  ensure_container_runtime

  local vision_port
  vision_port="$(ports_get "$name" vision_service)" || true
  if [[ -z "$vision_port" ]]; then
    say "no vision_service port allocated for $name (skipping vision sidecar)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" "$VISION_SIDECAR_IMAGE"; then
    say "vision sidecar image not available; run 'build-vision-sidecar' first (skipping)"
    return 0
  fi

  local container_name="lunarwing-vision-$name"

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-vision-${name}.container"
    render_vision_quadlet "$name"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-vision-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-vision-${name}.service" "lunarwing-vision-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-vision-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-vision-${name}" "$quadlet_file"
      say "vision sidecar ready via quadlet (lunarwing-vision-${name}.service, port $vision_port)"
    else
      say "WARNING: lunarwing-vision-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-vision-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "vision sidecar already running ($container_name, port $vision_port)"
    else
      _ctr "$name" start "$container_name" >/dev/null
      say "vision sidecar restarted ($container_name, port $vision_port)"
    fi
  else
    local vision_env_path
    vision_env_path="$(tenant_env_dir "$name")/vision.env"
    [[ -f "$vision_env_path" ]] || write_tenant_vision_env "$name" >/dev/null

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)

    # v10: also publish the tenant's dedicated vision_health port -> container
    # 8089, so the host self-heal pipeline can probe /health directly (mirrors
    # the external-worker health_publish pattern).
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" vision_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:${VISION_SIDECAR_HEALTH_PORT}")

    say "creating vision sidecar container $container_name on port $vision_port"
    _ctr "$name" run -d \
      --name "$container_name" \
      --env-file "$vision_env_path" \
      -p "127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}" \
      "${health_publish[@]}" \
      "${restart_arg[@]}" \
      "$VISION_SIDECAR_IMAGE" >/dev/null
    say "vision sidecar ready ($container_name, port $vision_port)"
  fi

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    render_vision_openrc_unit "$name"
    rc-update add "$container_name" default >/dev/null 2>&1 || true
    rc-service "$container_name" start >/dev/null 2>&1 || \
      say "WARNING: rc-service $container_name start returned non-zero (container may already be up)"
  fi
}

stop_tenant_vision() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-vision-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "vision sidecar stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "vision sidecar stopped ($container_name)"
  fi
}
