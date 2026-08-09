#!/usr/bin/env bash
# 07-units-systemd.sh — systemd unit rendering and lifecycle
# Extracted from lunarwing-mt-admin.sh by sunburst/mtadmin-refactor-v2.0.6.0
# shellcheck shell=bash

render_vision_quadlet() {
  local name="$1"
  local qdir vision_port vision_health_port env_path token
  qdir="$(tenant_quadlet_dir "$name")"
  vision_port="$(ports_get "$name" vision_service)"
  [[ -n "$vision_port" ]] || return 0
  vision_health_port="$(ports_get "$name" vision_health)" || true
  env_path="$(tenant_env_dir "$name")/vision.env"
  [[ -f "$env_path" ]] || write_tenant_vision_env "$name" >/dev/null
  token="$(_read_env_value "$env_path" LUNARWING_AUTH_TOKEN)"
  token="${token//%/%%}"
  mkdir -p "$qdir"

  {
    cat <<EOF
[Unit]
Description=LunarWing vision sidecar ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-vision-${name}
Image=${VISION_SIDECAR_IMAGE}
PublishPort=127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}
Environment=OCR_PORT=${VISION_SIDECAR_INTERNAL_PORT}
Environment=OCR_HEALTH_PORT=${VISION_SIDECAR_HEALTH_PORT}
Environment=LUNARWING_AUTH_TOKEN=${token}
EOF
    # Publish the per-tenant dedicated vision health port (v10) -> container's
    # 8089, so the host self-heal pipeline can probe /health independently of
    # OCR traffic. The sidecar listens on OCR_HEALTH_PORT=8089 internally.
    [[ -n "$vision_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:%s\n' "$vision_health_port" "$VISION_SIDECAR_HEALTH_PORT"
    if [[ -f "$env_path" ]]; then
      printf 'EnvironmentFile=%s\n' "$env_path"
    fi

    cat <<EOF

[Service]
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
EOF
  } > "$qdir/lunarwing-vision-${name}.container"
}

# ── LunarVoice ONNX sidecar ──────────────────────────────────────────────────

VOICE_SIDECAR_IMAGE=lunarwing/voice-service:latest
VOICE_SIDECAR_INTERNAL_PORT=8090
VOICE_SIDECAR_HEALTH_PORT=8091

build_voice_sidecar_image() {
  ensure_container_runtime
  local sidecar_dir="$SOURCE_REPO/projects/voice-sidecar"
  [[ -f "$sidecar_dir/Dockerfile" ]] \
    || { say "WARNING: LunarVoice Dockerfile missing: $sidecar_dir"; return 1; }
  "$CONTAINER_RT" build --network=host --format docker \
    -t "$VOICE_SIDECAR_IMAGE" -f "$sidecar_dir/Dockerfile" "$sidecar_dir" >/dev/null \
    || { say "WARNING: LunarVoice image build failed"; return 1; }
  say "LunarVoice sidecar image built: $VOICE_SIDECAR_IMAGE"
}

start_tenant_voice() {
  local name="$1"
  tenant_voice_enabled "$name" || { say "LunarVoice disabled for $name (skipping)"; return 0; }
  voice_models_ready "$name" \
    || { say "ERROR: verified Whisper models missing under $VOICE_MODELS_DIR" >&2; return 1; }
  ensure_container_runtime

  # SELinux: the shared model store must carry a container-readable label or
  # the sidecar gets EACCES on /models. chcon (not :z) because the dir is
  # root-owned and shared across tenants — a per-tenant relabel would fight.
  if command -v getenforce >/dev/null 2>&1 && [[ "$(getenforce)" == "Enforcing" ]]; then
    chcon -Rt container_file_t "$VOICE_MODELS_DIR" 2>/dev/null || true
  fi

  local voice_port voice_health_port container_name env_path
  voice_port="$(ports_get "$name" voice_service)" || true
  voice_health_port="$(ports_get "$name" voice_health)" || true
  [[ -n "$voice_port" && -n "$voice_health_port" ]] \
    || { say "ERROR: LunarVoice ports missing for $name" >&2; return 1; }
  if ! _ensure_tenant_image "$name" "$VOICE_SIDECAR_IMAGE"; then
    say "ERROR: LunarVoice image unavailable; run 'build-voice-sidecar' first" >&2
    return 1
  fi
  container_name="lunarwing-voice-$name"
  env_path="$(tenant_env_dir "$name")/voice.env"
  [[ -f "$env_path" ]] || write_tenant_voice_env "$name" >/dev/null

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-voice-${name}.container"
    render_voice_quadlet "$name"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "$container_name" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "${container_name}.service" "$container_name"
    fi
    if _systemctl_user "$name" start "${container_name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "$container_name" "$quadlet_file"
      say "LunarVoice ready via quadlet (${container_name}.service, port $voice_port)"
    else
      say "WARNING: ${container_name}.service failed to start" >&2
      _systemctl_user "$name" status "${container_name}.service" --no-pager >&2 || true
      return 1
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" start "$container_name" >/dev/null 2>&1 || true
  else
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    _ctr "$name" run -d \
      --name "$container_name" \
      --env-file "$env_path" \
      -p "127.0.0.1:${voice_port}:${VOICE_SIDECAR_INTERNAL_PORT}" \
      -p "127.0.0.1:${voice_health_port}:${VOICE_SIDECAR_HEALTH_PORT}" \
      -v "$VOICE_MODELS_DIR:/models:ro" \
      --security-opt=no-new-privileges \
      "${restart_arg[@]}" \
      "$VOICE_SIDECAR_IMAGE" >/dev/null
  fi
  say "LunarVoice ready ($container_name, port $voice_port)"

  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    render_voice_openrc_unit "$name"
    rc-update add "$container_name" default >/dev/null 2>&1 || true
    rc-service "$container_name" start >/dev/null 2>&1 \
      || { say "ERROR: rc-service $container_name start failed" >&2; return 1; }
  fi
}

stop_tenant_voice() {
  local name="$1" container_name="lunarwing-voice-$1"
  ensure_container_runtime
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "$OPENRC_INITD_DIR/${container_name}" ]]; then
    rc-service "$container_name" stop >/dev/null 2>&1 || true
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
  fi
  say "LunarVoice stopped ($container_name)"
}

render_voice_quadlet() {
  local name="$1" qdir voice_port voice_health_port env_path
  qdir="$(tenant_quadlet_dir "$name")"
  voice_port="$(ports_get "$name" voice_service)"
  voice_health_port="$(ports_get "$name" voice_health)"
  env_path="$(tenant_env_dir "$name")/voice.env"
  [[ -f "$env_path" ]] || write_tenant_voice_env "$name" >/dev/null
  mkdir -p "$qdir"
  cat >"$qdir/lunarwing-voice-${name}.container" <<EOF
[Unit]
Description=LunarVoice ONNX sidecar ($name)
After=network-online.target
Wants=network-online.target
Before=lunarwing-${name}.service
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-voice-${name}
Image=${VOICE_SIDECAR_IMAGE}
PublishPort=127.0.0.1:${voice_port}:${VOICE_SIDECAR_INTERNAL_PORT}
PublishPort=127.0.0.1:${voice_health_port}:${VOICE_SIDECAR_HEALTH_PORT}
EnvironmentFile=${env_path}
Volume=${VOICE_MODELS_DIR}:/models:ro
SecurityLabelDisable=false

[Service]
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
EOF
}

render_voice_openrc_unit() {
  local name="$1" runtime_bin container uid home voice_health_port
  container="lunarwing-voice-${name}"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"
  voice_health_port="$(ports_get "$name" voice_health)"
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  cat >"$OPENRC_INITD_DIR/${container}" <<INITEOF
#!/sbin/openrc-run
description="LunarVoice sidecar ($name)"
: "\${voice_runtime:=$runtime_bin}"
: "\${voice_container:=$container}"
: "\${voice_rootless:=$MT_ROOTLESS}"
: "\${voice_user:=$name}"
: "\${voice_home:=$home}"
: "\${voice_uid:=$uid}"
: "\${voice_health_port:=$voice_health_port}"

depend() { need net localmount; after firewall; before lunarwing-${name}; }
_voice() {
    if [ "\${voice_rootless}" = "true" ]; then
        sudo -u "\${voice_user}" env HOME="\${voice_home}" XDG_RUNTIME_DIR="/run/user/\${voice_uid}" "\${voice_runtime}" "\$@"
    else
        "\${voice_runtime}" "\$@"
    fi
}
_healthy() {
    [ "\$(_voice inspect -f '{{.State.Running}}' "\${voice_container}" 2>/dev/null)" = "true" ] || return 1
    curl -fsS --max-time 3 "http://127.0.0.1:\${voice_health_port}/health" >/dev/null 2>&1
}
start() {
    ebegin "Starting LunarVoice (\${voice_container})"
    _voice start "\${voice_container}" >/dev/null 2>&1 || { eend 1; return 1; }
    _wait=0
    while ! _healthy; do
        _wait=\$((_wait + 1)); [ "\$_wait" -lt 60 ] || { eend 1 "not healthy"; return 1; }
        sleep 1
    done
    eend 0
}
stop() {
    ebegin "Stopping LunarVoice (\${voice_container})"
    _voice stop --time 30 "\${voice_container}" >/dev/null 2>&1
    eend \$?
}
status() { _healthy && { einfo "\${voice_container}: started"; return 0; }; einfo "\${voice_container}: stopped"; return 3; }
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/${container}"
}

configure_voice() {
  local name="$1" enabled="$2" voice_env lunarwing_env token voice_port stt_model
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  voice_env="$(tenant_env_dir "$name")/voice.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$voice_env" ]] || write_tenant_voice_env "$name" >/dev/null
  [[ -f "$lunarwing_env" ]] || die "tenant LunarWing environment missing"
  if [[ "$enabled" == "true" ]]; then
    voice_models_ready "$name" || die "verified Whisper models missing under $VOICE_MODELS_DIR"
  fi
  token="$(_env_existing "$voice_env" VOICE_AUTH_TOKEN)"
  stt_model="$(_env_existing "$voice_env" VOICE_STT_MODEL)"
  stt_model="${stt_model:-whisper-tiny}"
  voice_port="$(ports_get "$name" voice_service)"
  _env_set "$voice_env" VOICE_ENABLED "$enabled"
  _env_set "$lunarwing_env" VOICE_SERVICE_URL "http://127.0.0.1:${voice_port}"
  _env_set "$lunarwing_env" VOICE_AUTH_TOKEN "$token"
  if [[ "$enabled" == true ]]; then
    _env_set "$lunarwing_env" TRANSCRIPTION_PROVIDER lunarvoice
    _env_set "$lunarwing_env" TRANSCRIPTION_MODEL "$stt_model"
    _env_set "$lunarwing_env" TRANSCRIPTION_ENABLED true
  elif [[ "$(_env_existing "$lunarwing_env" TRANSCRIPTION_PROVIDER)" == lunarvoice ]]; then
    _env_set "$lunarwing_env" TRANSCRIPTION_ENABLED false
  fi
  ports_set_voice "$name" "$enabled"
  say "LunarVoice enabled=$enabled for $name; restart tenant to apply daemon transcription settings"
}

remove_tenant_voice_unit() {
  local name="$1" container_name="lunarwing-voice-$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    rm -f "$(tenant_quadlet_dir "$name")/${container_name}.container"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
  else
    rc-update del "$container_name" default >/dev/null 2>&1 || true
    rm -f "$OPENRC_INITD_DIR/$container_name"
  fi
}

# ── PostgreSQL container ─────────────────────────────────────────────────────

render_pg_quadlet() {
  local name="$1"
  local qdir pg_port pg_password
  qdir="$(tenant_quadlet_dir "$name")"
  pg_port="$(ports_get "$name" postgres)"
  pg_password="$(tenant_pg_password "$name")"   # inlined into the 0600 tenant-owned .container
  mkdir -p "$qdir"
  cat >"$qdir/lunarwing-pg-${name}.container" <<EOF
[Unit]
Description=LunarWing Postgres container ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-pg-${name}
Image=${PG_IMAGE}
PublishPort=127.0.0.1:${pg_port}:5432
Volume=lunarwing-pg-${name}:/var/lib/postgresql/data
Environment=POSTGRES_USER=lunarwing
Environment=POSTGRES_PASSWORD=${pg_password}
Environment=POSTGRES_DB=lunarwing
HealthCmd=pg_isready -U lunarwing -q
HealthInterval=10s
HealthTimeout=3s
HealthRetries=5
HealthStartPeriod=30s

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  chmod 0600 "$qdir/lunarwing-pg-${name}.container"
  chown -R "$name:$name" "$(tenant_home "$name")/.config/containers"
}

# Render a per-tenant Quadlet .container for an external worker.
# Mirrors the imperative env from start_tenant_<worker>: the GATEWAY_AUTH_TOKEN ->
# AGENT_AUTH_TOKEN and LLM_API_KEY -> TENSORZERO_API_KEY remap is resolved here and
# inlined as Environment= in a 0600 tenant-owned unit. Kimi uses a separate
# mode-0600 runtime env file so its API key never enters generated unit commands.
# Returns early (no unit) when the worker has no allocated wss port.
# (render_worker_quadlet moved to mt-admin-lib/09-worker-config.sh)

# Render the WeeChat systemd user unit into <out_dir>.
# Loads only weechat.env (RELAY_PASSWORD), not the full lunarwing.env.
# ExecStop sends /upgrade -quit via the WeeChat FIFO so WeeChat saves its
# session (buffers, lines, connections) before exiting; next start restores
# it. Falls back to tmux kill-session if the graceful stop fails.
_render_weechat_systemd_unit() {  # <tenant> <out_dir>
  local name="$1" out_dir="$2"
  local weechat_home env_dir start_helper stop_helper weechat_binary
  weechat_home="$(tenant_weechat_home "$name")"
  env_dir="$(tenant_env_dir "$name")"
  start_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-start.sh"
  stop_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-stop.sh"
  weechat_binary="$(command -v weechat)"
  cat >"$out_dir/lunarwing-weechat-${name}.service" <<EOF
[Unit]
Description=WeeChat IRC client ($name)
After=network.target

[Service]
Type=forking
ExecStart=$(command -v tmux) -L weechat-${name} new-session -d -s weechat '$start_helper --weechat-bin $weechat_binary --weechat-home ${weechat_home}'
ExecStop=$stop_helper --weechat-home ${weechat_home} --tmux-socket weechat-${name} --session weechat
TimeoutStopSec=30
EnvironmentFile=$env_dir/weechat.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
}

render_tenant_systemd_units() {
  local name="$1"
  ensure_container_runtime
  local user_unit_dir
  user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
  mkdir -p "$user_unit_dir"
  chown -R "$name:$name" "$(tenant_home "$name")/.config"

  local repo env_dir state_dir proxy_port bridge_port weechat_port
  repo="$(tenant_repo "$name")"
  env_dir="$(tenant_env_dir "$name")"
  state_dir="$(tenant_state_dir "$name")"
  proxy_port="$(ports_get "$name" proxy)"
  bridge_port="$(ports_get "$name" bridge)"
  weechat_port="$(ports_get "$name" weechat)"

  local proxy_bin
  # Run from the tenant's own clone (in their home), not the admin's source repo,
  # so a tenant's services aren't coupled to another user's home directory.
  proxy_bin="$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"

  local ws_adapter_path
  ws_adapter_path="$(tenant_lw_root "$name")/lunarwing_weechat_wss/weechat_relay/ws_adapter.py"
  local weechat_enabled=false
  tenant_weechat_enabled "$name" && weechat_enabled=true

  # Proxy unit (only when enabled)
  if tenant_proxy_enabled "$name"; then
  cat >"$user_unit_dir/lunarwing-proxy-${name}.service" <<EOF
[Unit]
Description=LunarWing TensorZero proxy ($name)
After=network.target

[Service]
Type=simple
ExecStart=$(command -v python3) $proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero $DEFAULT_TENSORZERO_URL
EnvironmentFile=$env_dir/proxy.env
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF
  fi

  if [[ "$weechat_enabled" == "true" ]]; then
    # WeeChat unit (runs in tmux so you can attach: tmux -L weechat-${name} attach)
    local weechat_home
    weechat_home="$(tenant_home "$name")/.config/weechat"
    mkdir -p "$weechat_home"
    chown "$name:$name" "$weechat_home"

    _render_weechat_systemd_unit "$name" "$user_unit_dir"

    # WeeChat WS adapter unit
    cat >"$user_unit_dir/lunarwing-weechat-adapter-${name}.service" <<EOF
[Unit]
Description=LunarWing WeeChat WS adapter ($name)
After=network.target lunarwing-weechat-${name}.service
Requires=lunarwing-weechat-${name}.service
PartOf=lunarwing-${name}.service

[Service]
Type=simple
WorkingDirectory=$(dirname "$ws_adapter_path")
EnvironmentFile=$env_dir/lunarwing.env
ExecStart=$(command -v python3) $ws_adapter_path
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF
  else
    # Remove stale files and enablement links so render-units also reconciles a
    # tenant whose registry flag was changed outside the lifecycle command.
    rm -f \
      "$user_unit_dir/lunarwing-weechat-${name}.service" \
      "$user_unit_dir/lunarwing-weechat-adapter-${name}.service" \
      "$user_unit_dir/default.target.wants/lunarwing-weechat-${name}.service" \
      "$user_unit_dir/default.target.wants/lunarwing-weechat-adapter-${name}.service"
  fi

  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_path
    darkirc_adapter_path="$(tenant_lw_root "$name")/darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py"

    cat >"$user_unit_dir/lunarwing-darkirc-adapter-${name}.service" <<EOF
[Unit]
Description=LunarWing DarkIRC adapter ($name)
After=network.target

[Service]
Type=simple
WorkingDirectory=$(dirname "$darkirc_adapter_path")
EnvironmentFile=$env_dir/darkirc-adapter.env
ExecStart=$(command -v python3) $darkirc_adapter_path
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

    # DarkIRC daemon unit
    cat >"$user_unit_dir/lunarwing-darkirc-${name}.service" <<EOF
[Unit]
Description=DarkIRC daemon ($name)
After=network.target

[Service]
Type=simple
ExecStart=$DARKIRC_RELEASE_DAEMON --config $(tenant_state_dir "$name")/darkirc/darkirc_config.toml
Restart=always
RestartSec=10
NoNewPrivileges=true
UMask=0077

[Install]
WantedBy=default.target
EOF
  fi

  # Bridge unit
  cat >"$user_unit_dir/xmpp-bridge-${name}.service" <<EOF
[Unit]
Description=LunarWing XMPP bridge ($name)
After=network.target
PartOf=lunarwing-${name}.service

[Service]
Type=simple
WorkingDirectory=$repo/bridges/xmpp-bridge
EnvironmentFile=$env_dir/xmpp-bridge.env
ExecStart=$repo/bridges/xmpp-bridge/target/${PROFILE}/xmpp-bridge
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

  # Postgres dependency — only when the pg Quadlet is rendered (systemd + rootless
  # podman with Quadlet support). Mirrors the OpenRC `need lunarwing-pg-<t>`.
  # Rootful docker has no pg unit (the container survives via --restart), so the
  # dependency is omitted there to avoid a Requires on a non-existent unit.
  local pg_dep_after="" pg_dep_requires=""
  if [[ "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    pg_dep_after="lunarwing-pg-${name}.service "
    pg_dep_requires="Requires=lunarwing-pg-${name}.service"
  fi

  local darkirc_sd_after="" darkirc_sd_wants=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_sd_after=" lunarwing-darkirc-adapter-${name}.service"
    darkirc_sd_wants=" lunarwing-darkirc-adapter-${name}.service"
  fi

  local proxy_sd_after="" proxy_sd_wants=""
  if tenant_proxy_enabled "$name"; then
    proxy_sd_after=" lunarwing-proxy-${name}.service"
    proxy_sd_wants=" lunarwing-proxy-${name}.service"
  fi

  local weechat_sd_after="" weechat_sd_wants=""
  if [[ "$weechat_enabled" == "true" ]]; then
    weechat_sd_after=" lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service"
    weechat_sd_wants=" lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service"
  fi

  # Main daemon unit
  cat >"$user_unit_dir/lunarwing-${name}.service" <<EOF
[Unit]
Description=LunarWing AI assistant ($name)
After=network.target ${pg_dep_after}xmpp-bridge-${name}.service${proxy_sd_after}${weechat_sd_after}${darkirc_sd_after}
Wants=xmpp-bridge-${name}.service${proxy_sd_wants}${weechat_sd_wants}${darkirc_sd_wants}
${pg_dep_requires}

[Service]
Type=simple
WorkingDirectory=$repo
EnvironmentFile=$env_dir/lunarwing.env
Environment=PATH=/usr/local/bin:/usr/bin:/bin:/home/${name}/.cargo/bin
ExecStart=$repo/target/${PROFILE}/lunarwing --no-onboard run
Restart=always
RestartSec=5
TimeoutStartSec=60
TimeoutStopSec=30
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
# BUG-005: the daemon is capped separately from its transient workloads;
# shell/compile jobs run in lunarwing-workloads.slice scopes (see
# ic/src/tools/process.rs), so their memory never lands here.
MemoryHigh=$DAEMON_MEMORY_HIGH
MemoryMax=$DAEMON_MEMORY_MAX
TasksMax=$DAEMON_TASKS_MAX

[Install]
WantedBy=default.target
EOF

  # BUG-005: transient workload slices. Agent-spawned shell/compile jobs run
  # as scopes under lunarwing-workloads.slice; operator-driven builds
  # (build_tenant) run under lunarwing-build.slice. Slice names are constant
  # across tenants because each tenant has its own systemd user manager.
  cat >"$user_unit_dir/$WORKLOAD_SLICE" <<EOF
[Unit]
Description=LunarWing transient agent workloads ($name)

[Slice]
MemoryHigh=$WORKLOAD_MEMORY_HIGH
MemoryMax=$WORKLOAD_MEMORY_MAX
CPUQuota=$WORKLOAD_CPU_QUOTA
TasksMax=$WORKLOAD_TASKS_MAX
EOF

  cat >"$user_unit_dir/$BUILD_SLICE" <<EOF
[Unit]
Description=LunarWing operator build workloads ($name)

[Slice]
MemoryHigh=$BUILD_MEMORY_HIGH
MemoryMax=$BUILD_MEMORY_MAX
EOF

  # The pg + worker Quadlet .container units are rendered by the start functions
  # (start_tenant_postgres / start_tenant_<worker>), NOT here: those guard on
  # image availability, so a not-yet-built worker image never produces a unit
  # that would crash-loop at boot (Restart=always). The daemon's Requires= above
  # still resolves because start_tenant_postgres renders + starts pg first.

  chown -R "$name:$name" "$user_unit_dir"
  say "rendered systemd units for $name in $user_unit_dir"
}

_systemctl_user() {
  local name="$1"
  shift
  local uid
  uid="$(id -u "$name")"
  sudo -u "$name" XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user "$@"
}

# Best-effort wait for the tenant's `systemd --user` manager + bus to be ready,
# so `systemctl --user` and the Quadlet generator work right after enable-linger
# (which can return before the user manager is fully up). Proceeds after ~10s.
_wait_user_manager() {
  local name="$1" uid i
  uid="$(id -u "$name" 2>/dev/null)" || return 0
  for i in $(seq 1 20); do
    [[ -S "/run/user/$uid/bus" ]] && return 0
    sleep 0.5
  done
  return 0
}

start_tenant_systemd() {
  local name="$1"
  _systemctl_user "$name" daemon-reload
  if tenant_darkirc_enabled "$name"; then
    darkirc_service_uses_release_selector "$name" \
      || die "effective DarkIRC systemd unit is stale or overridden; run render-units $name"
  fi
  # Quadlet-generated units (pg, supported workers, vision) are NOT in the
  # enable_list: `systemctl enable` fails on generated/transient units with
  # "Failed to enable unit: ... is transient or generated", and under
  # `set -euo pipefail` that non-zero exit aborts the entire enable batch —
  # leaving every regular service disabled. Each Quadlet unit is already
  # started by its own start_tenant_* function (which renders the quadlet,
  # reloads the daemon, and starts the unit). The imperative `start` below
  # is belt-and-suspenders for the vision unit (harmless if already running).
  local enable_list=("lunarwing-${name}.service" "xmpp-bridge-${name}.service")
  # WeeChat units join the enable list only when the tenant's registry flag
  # allows them (ISSUE-006): a disabled tenant must not be re-enabled for
  # future boots by start-tenant.
  if tenant_weechat_enabled "$name"; then
    enable_list+=("lunarwing-weechat-${name}.service" "lunarwing-weechat-adapter-${name}.service")
  fi
  if tenant_proxy_enabled "$name"; then
    enable_list+=("lunarwing-proxy-${name}.service")
  fi
  if tenant_darkirc_enabled "$name"; then
    enable_list+=("lunarwing-darkirc-${name}.service" "lunarwing-darkirc-adapter-${name}.service")
  fi
  _systemctl_user "$name" enable "${enable_list[@]}"
  if tenant_darkirc_enabled "$name"; then
    _systemctl_user "$name" start "lunarwing-darkirc-${name}.service"
    sleep 2
  fi
  _systemctl_user "$name" start "lunarwing-vision-${name}.service" 2>/dev/null || true
  _systemctl_user "$name" start "lunarwing-${name}.service"
  sleep 2
  if _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service"; then
    say "lunarwing-${name}.service is active"
  else
    say "WARNING: lunarwing-${name}.service failed to start" >&2
    _systemctl_user "$name" status "lunarwing-${name}.service" --no-pager >&2 || true
    return 1
  fi
}

stop_tenant_systemd() {
  local name="$1"
  local uid
  uid="$(id -u "$name" 2>/dev/null)" || return 0

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service" "lunarwing-nanocode-${name}.service" "lunarwing-pebble-${name}.service" "lunarwing-opencode-${name}.service" "lunarwing-kimi-${name}.service" "lunarwing-vision-${name}.service" "lunarwing-pg-${name}.service"; do
    if _systemctl_user "$name" is-active --quiet "$svc" 2>/dev/null; then
      _systemctl_user "$name" stop "$svc"
      say "stopped $svc"
    fi
  done
}

uninstall_tenant_systemd() {
  local name="$1"
  local user_unit_dir
  user_unit_dir="$(tenant_home "$name")/.config/systemd/user"

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service"; do
    rm -f "$user_unit_dir/$svc"
  done

  # Quadlet .container units (rootless pg + workers + vision sidecar). Remove the
  # worker containers (their workspace data is bind-mounted in the home); the pg
  # container + named volume are handled by stop_tenant_postgres /
  # reset_tenant_postgres so non-purge removals keep the data for a later re-add.
  local qdir; qdir="$(tenant_quadlet_dir "$name")"
  rm -f "$qdir/lunarwing-pg-${name}.container" \
        "$qdir/lunarwing-nanocode-${name}.container" \
        "$qdir/lunarwing-pebble-${name}.container" \
        "$qdir/lunarwing-opencode-${name}.container" \
        "$qdir/lunarwing-kimi-${name}.container" \
        "$qdir/lunarwing-vision-${name}.container"
  if id -u "$name" >/dev/null 2>&1; then
    # Retirement preserves stopped Nanocode containers for rollback and manual
    # recovery. Supported workers remain disposable because their state lives
    # in bind-mounted workspaces.
    for w in pebble opencode kimi; do
      _ctr "$name" rm -f "lunarwing-${w}-${name}" >/dev/null 2>&1 || true
    done
    _ctr "$name" rm -f "lunarwing-vision-${name}" >/dev/null 2>&1 || true
  fi

  _systemctl_user "$name" daemon-reload 2>/dev/null || true
  say "uninstalled systemd units for $name"
}
