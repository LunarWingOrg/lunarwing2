#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# units-systemd.sh — part of the lunarwing-mt-admin library.
#
# systemd unit + quadlet rendering and tenant service lifecycle. This is
# the sanctioned surface for tenant systemctl calls: _systemctl_user is
# the only exit point (see the HARD RULE in ic/AGENTS.md) — no caller
# outside this file may invoke systemctl against tenant units.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.

render_vision_quadlet() {
  local name="$1"
  local qdir vision_port vision_health_port env_path token
  qdir="$(tenant_quadlet_dir "$name")"
  vision_port="$(ports_get "$name" vision_service)"
  [[ -n "$vision_port" ]] || return 0
  vision_health_port="$(ports_get "$name" vision_health)" || true
  env_path="$(tenant_env_dir "$name")/vision.env"
  [[ -f "$env_path" ]] || write_tenant_vision_env "$name" >/dev/null
  token="$(grep '^LUNARWING_AUTH_TOKEN=' "$env_path" 2>/dev/null | cut -d= -f2- || true)"
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

# ── Systemd service units ────────────────────────────────────────────────────

# Render a per-tenant Quadlet .container for the Postgres container. The podman
# user-generator turns this into lunarwing-pg-<t>.service at `systemctl --user
# daemon-reload`; [Install] makes the lingering user manager start it at boot.
# Quadlet owns creation (the imperative `_ctr run` is skipped on this path), so
# the named volume below preserves data across container replacement.
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

# Render a per-tenant Quadlet .container for an external worker (nanocode/pebble).
# Mirrors the imperative env from start_tenant_<worker>: the GATEWAY_AUTH_TOKEN ->
# AGENT_AUTH_TOKEN and LLM_API_KEY -> TENSORZERO_API_KEY remap is resolved here and
# inlined as Environment= in a 0600 tenant-owned unit (no secret leaves the file).
# Returns early (no unit) when the worker has no allocated wss port.
render_worker_quadlet() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  local qdir wss_port workspace_dir env_dir tenant_env_path worker_env_path host_health_port
  qdir="$(tenant_quadlet_dir "$name")"
  wss_port="$(ports_get "$name" "${worker}_wss")"
  host_health_port="$(ports_get "$name" "${worker}_health")"
  [[ -n "$wss_port" ]] || return 0
  env_dir="$(tenant_env_dir "$name")"
  tenant_env_path="$env_dir/lunarwing.env"
  worker_env_path="$env_dir/${worker}.env"
  workspace_dir="$(tenant_lw_root "$name")/${worker}-workspace"
  mkdir -p "$workspace_dir"; chown "$name:$name" "$workspace_dir"; chmod 777 "$workspace_dir"
  mkdir -p "$qdir"

  local agent_token tz_key
  agent_token="$(grep '^GATEWAY_AUTH_TOKEN=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  tz_key="$(grep '^LLM_API_KEY=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  local nanocode_model nanocode_base_url
  nanocode_model="$(grep '^NANOCODE_MODEL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  nanocode_base_url="$(grep '^NANOCODE_BASE_URL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
  # systemd treats % as a unit specifier; escape so a token containing % survives.
  agent_token="${agent_token//%/%%}"
  tz_key="${tz_key//%/%%}"

  # SSH agent socket: the gateway creates an SSH agent server at startup,
  # with its socket at <tenant_home>/lunarwing/run/ssh-agent.sock (NOT /tmp,
  # because the daemon runs with PrivateTmp=true — a /tmp socket would be
  # invisible to podman containers). Always include the volume mount + env var
  # in the quadlet, even if the socket doesn't exist yet at render time — the
  # daemon creates it before the worker needs it (workers only use SSH when a
  # create_job with SSH commands is invoked, well after startup). If SSH is
  # not configured for the tenant, the socket simply won't exist and SSH
  # commands from the worker will fail with a clear "no agent" error.
  local ssh_agent_socket
  ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"

  {
    cat <<EOF
[Unit]
Description=LunarWing ${worker} worker ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-${worker}-${name}
Image=lunarwing-worker-${worker}:latest
PublishPort=127.0.0.1:${wss_port}:${wss_port}
Volume=${workspace_dir}:/workspace:z
Environment=LUNARWING_WORKER_ID=worker-${worker}-${name}
Environment=WS_PORT=${wss_port}
Environment=HEALTH_PORT=${health_port}
Environment=WS_BIND_HOST=0.0.0.0
Environment=WS_PATH=/ws/agent
EOF
    # SSH agent socket mount + env (always included — the daemon creates the
    # socket at startup; if SSH isn't configured, the socket won't exist and
    # SSH commands from the worker will fail with a clear "no agent" error).
    # :z label so SELinux (Enforcing on Fedora) permits container_t access to
    # the tenant-home-labeled socket.
    printf 'Volume=%s:/tmp/ssh-agent.sock:z\n' "$ssh_agent_socket"
    printf 'Environment=SSH_AUTH_SOCK=/tmp/ssh-agent.sock\n'
    # Publish the per-tenant dedicated health port (v8) -> container's 8443, so
    # the host self-heal pipeline can probe /health directly. The container still
    # listens on HEALTH_PORT=8443 internally (matches the image's baked HEALTHCHECK).
    [[ -n "$host_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:8443\n' "$host_health_port"
    if [[ "$worker" == "nanocode" ]]; then
      printf 'Environment=NANOCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    elif [[ "$worker" == "pebble" ]]; then
      printf 'Environment=PEBBLE_MODE=websocket\n'
    elif [[ "$worker" == "opencode" ]]; then
      printf 'Environment=OPENCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    fi
    [[ -n "$agent_token" ]] && printf 'Environment=AGENT_AUTH_TOKEN=%s\n' "$agent_token"
    [[ "$worker" == "nanocode" && -n "$tz_key" ]] && printf 'Environment=TENSORZERO_API_KEY=%s\n' "$tz_key"
    [[ "$worker" == "nanocode" && -n "$nanocode_model" ]] && printf 'Environment=NANOCODE_MODEL=%s\n' "$nanocode_model"
    [[ "$worker" == "nanocode" && -n "$nanocode_base_url" ]] && printf 'Environment=NANOCODE_BASE_URL=%s\n' "$nanocode_base_url"
    [[ "$worker" == "opencode" && -n "$tz_key" ]] && printf 'Environment=TENSORZERO_API_KEY=%s\n' "$tz_key"
    local oc_model oc_base_url
    oc_model="$(grep '^OPENCODE_MODEL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
    oc_base_url="$(grep '^OPENCODE_BASE_URL=' "$tenant_env_path" 2>/dev/null | cut -d= -f2- || true)"
    [[ "$worker" == "opencode" && -n "$oc_model" ]] && printf 'Environment=OPENCODE_MODEL=%s\n' "$oc_model"
    [[ "$worker" == "opencode" && -n "$oc_base_url" ]] && printf 'Environment=OPENCODE_BASE_URL=%s\n' "$oc_base_url"
    # Operator override file (optional). EnvironmentFile= has existed since the
    # Quadlet 4.4 debut, so it is safe at our >= 4.6 floor.
    [[ -f "$worker_env_path" ]] && printf 'EnvironmentFile=%s\n' "$worker_env_path"
    [[ "$worker" == "nanocode" || "$worker" == "opencode" ]] && printf 'Exec=--mode websocket\n'
    cat <<EOF
HealthCmd=curl -sf http://127.0.0.1:${health_port}/health || exit 1
HealthInterval=15s
HealthTimeout=5s
HealthRetries=3
HealthStartPeriod=30s

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  } >"$qdir/lunarwing-${worker}-${name}.container"
  chmod 0600 "$qdir/lunarwing-${worker}-${name}.container"
  chown -R "$name:$name" "$(tenant_home "$name")/.config/containers"
}

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
ExecStart=$DARKIRC_BIN --config $(tenant_state_dir "$name")/darkirc/darkirc_config.toml
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

  # Main daemon unit
  cat >"$user_unit_dir/lunarwing-${name}.service" <<EOF
[Unit]
Description=LunarWing AI assistant ($name)
After=network.target ${pg_dep_after}xmpp-bridge-${name}.service${proxy_sd_after} lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service${darkirc_sd_after}
  Wants=xmpp-bridge-${name}.service${proxy_sd_wants} lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service${darkirc_sd_wants}
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

[Install]
WantedBy=default.target
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
  # Quadlet-generated units (pg, nanocode, pebble, vision) are NOT in the
  # enable_list: `systemctl enable` fails on generated/transient units with
  # "Failed to enable unit: ... is transient or generated", and under
  # `set -euo pipefail` that non-zero exit aborts the entire enable batch —
  # leaving every regular service disabled. Each Quadlet unit is already
  # started by its own start_tenant_* function (which renders the quadlet,
  # reloads the daemon, and starts the unit). The imperative `start` below
  # is belt-and-suspenders for the vision unit (harmless if already running).
  local enable_list=("lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-weechat-adapter-${name}.service")
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

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service" "lunarwing-nanocode-${name}.service" "lunarwing-pebble-${name}.service" "lunarwing-opencode-${name}.service" "lunarwing-vision-${name}.service" "lunarwing-pg-${name}.service"; do
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
        "$qdir/lunarwing-vision-${name}.container"
  if id -u "$name" >/dev/null 2>&1; then
    for w in nanocode pebble opencode; do
      _ctr "$name" rm -f "lunarwing-${w}-${name}" >/dev/null 2>&1 || true
    done
    _ctr "$name" rm -f "lunarwing-vision-${name}" >/dev/null 2>&1 || true
  fi

  _systemctl_user "$name" daemon-reload 2>/dev/null || true
  say "uninstalled systemd units for $name"
}

# Render the WeeChat OpenRC init script into <out_file>.
