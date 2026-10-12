#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 07-units-systemd.sh — part of the lunarwing-mt-admin library.
#
# systemd user-unit rendering (daemon, postgres, worker, weechat, vision/voice
# quadlets + fallback units) and systemd tenant lifecycle. _systemctl_user is
# the sole tenant systemctl path.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


_remove_vision_systemd_fallback() {
  local name="$1" service="lunarwing-vision-${1}.service" path
  path="$(tenant_home "$name")/.config/systemd/user/$service"
  [[ ! -e "$path" && ! -L "$path" ]] && return 0
  if [[ ! -f "$path" || -L "$path" ]] \
      || ! grep -q '^# Managed by lunarwing-mt-admin: LunarVision fallback$' "$path"; then
    say "ERROR: refusing to remove unmanaged systemd unit $path" >&2
    return 1
  fi
  _systemctl_user "$name" disable --now "$service" >/dev/null 2>&1 || true
  rm -f -- "$path"
}

remove_vision_systemd_assets() {
  local name="$1" container="lunarwing-vision-${1}" qfile
  qfile="$(tenant_quadlet_dir "$name")/${container}.container"
  _systemctl_user "$name" stop "${container}.service" >/dev/null 2>&1 || true
  _remove_vision_systemd_fallback "$name" || return 1
  if [[ -e "$qfile" || -L "$qfile" ]]; then
    [[ -f "$qfile" && ! -L "$qfile" ]] \
      || { say "ERROR: refusing to remove unsafe LunarVision Quadlet $qfile" >&2; return 1; }
    rm -f -- "$qfile"
  fi
  rm -f -- "$(_hash_dir "$name")/${container}.hash"
  _systemctl_user "$name" daemon-reload >/dev/null 2>&1 || true
}

render_vision_systemd_fallback_unit() {
  local name="$1" container="lunarwing-vision-${1}" unit_dir path tmp runtime_bin babysitter home uid
  unit_dir="$(tenant_home "$name")/.config/systemd/user"
  path="$unit_dir/${container}.service"
  runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  babysitter="${LUNARWING_MT_BABYSITTER_HELPER:-/usr/local/sbin/lunarwing-ctr-babysit}"
  home="$(tenant_home "$name")"
  uid="$(id -u "$name" 2>/dev/null || true)"
  [[ "$CONTAINER_RT" == "podman" && -n "$runtime_bin" && -n "$uid" ]] || return 1
  ensure_babysitter_helper
  [[ -x "$babysitter" ]] || return 1
  if [[ -e "$path" || -L "$path" ]]; then
    if [[ ! -f "$path" || -L "$path" ]] \
        || ! grep -q '^# Managed by lunarwing-mt-admin: LunarVision fallback$' "$path"; then
      say "ERROR: refusing to overwrite unmanaged systemd unit $path" >&2
      return 1
    fi
  fi
  mkdir -p "$unit_dir"
  tmp="$(mktemp "$unit_dir/.${container}.service.XXXXXX")" || return 1
  cat >"$tmp" <<EOF
# Managed by lunarwing-mt-admin: LunarVision fallback
[Unit]
Description=LunarWing vision sidecar ($name)
After=network-online.target
Wants=network-online.target
Before=lunarwing-${name}.service
StartLimitIntervalSec=300
StartLimitBurst=10

[Service]
Type=simple
Environment=HOME=${home}
Environment=XDG_RUNTIME_DIR=/run/user/${uid}
ExecStart=${babysitter} ${container}
ExecStop=${runtime_bin} stop --time 30 ${container}
Restart=always
RestartSec=2
TimeoutStopSec=45

[Install]
WantedBy=default.target
EOF
  chown "$name:$name" "$tmp" || { rm -f -- "$tmp"; return 1; }
  chmod 0644 "$tmp" || { rm -f -- "$tmp"; return 1; }
  mv -f -- "$tmp" "$path"
}

_prepare_vision_systemd_fallback() {
  local name="$1" container="lunarwing-vision-${1}" qfile
  qfile="$(tenant_quadlet_dir "$name")/${container}.container"
  [[ ! -e "$qfile" && ! -L "$qfile" ]] && return 0
  _systemctl_user "$name" stop "${container}.service" >/dev/null 2>&1 || true
  rm -f -- "$qfile"
  _systemctl_user "$name" daemon-reload >/dev/null 2>&1 || return 1
}

_register_vision_systemd_fallback() {
  local name="$1" container="lunarwing-vision-${1}"
  render_vision_systemd_fallback_unit "$name" || return 1
  _systemctl_user "$name" daemon-reload >/dev/null 2>&1 || return 1
  _systemctl_user "$name" reset-failed "${container}.service" >/dev/null 2>&1 || true
  _systemctl_user "$name" enable --now "${container}.service" >/dev/null 2>&1 || return 1
  _wait_tenant_vision_healthy "$name" "$container"
}

render_vision_quadlet() {
  local name="$1" qdir vision_port vision_health_port env_path path tmp
  qdir="$(tenant_quadlet_dir "$name")"
  vision_port="$(ports_get "$name" vision_service)" || return 1
  vision_health_port="$(ports_get "$name" vision_health)" || return 1
  _vision_port_valid "$vision_port" && _vision_port_valid "$vision_health_port" || return 1
  env_path="$(tenant_env_dir "$name")/vision.env"
  [[ -f "$env_path" && ! -L "$env_path" ]] \
    || { say "ERROR: regular LunarVision env file required: $env_path" >&2; return 1; }
  mkdir -p "$qdir"
  path="$qdir/lunarwing-vision-${name}.container"
  if [[ -e "$path" || -L "$path" ]]; then
    [[ -f "$path" && ! -L "$path" ]] \
      || { say "ERROR: refusing to overwrite unsafe Quadlet path $path" >&2; return 1; }
  fi
  tmp="$(mktemp "$qdir/.lunarwing-vision-${name}.container.XXXXXX")" || return 1

  cat >"$tmp" <<EOF
[Unit]
Description=LunarWing vision sidecar ($name)
After=network-online.target
Wants=network-online.target
Before=lunarwing-${name}.service
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-vision-${name}
Image=${VISION_SIDECAR_IMAGE}
PublishPort=127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}
PublishPort=127.0.0.1:${vision_health_port}:${VISION_SIDECAR_HEALTH_PORT}
EnvironmentFile=${env_path}
HealthCmd=curl -sf http://127.0.0.1:${VISION_SIDECAR_HEALTH_PORT}/health || exit 1
HealthInterval=15s
HealthTimeout=5s
HealthRetries=3
HealthStartPeriod=30s
PodmanArgs=--security-opt=no-new-privileges
SecurityLabelDisable=false

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  chown "$name:$name" "$tmp" || { rm -f -- "$tmp"; return 1; }
  chmod 0644 "$tmp" || { rm -f -- "$tmp"; return 1; }
  mv -f -- "$tmp" "$path"
}

render_vision_systemd_assets() {
  local name="$1" container qfile fallback has_quadlet=false has_fallback=false
  if tenant_vision_external "$name"; then
    remove_vision_systemd_assets "$name"
    return
  fi
  ensure_container_runtime
  [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]] || return 0
  container="lunarwing-vision-${name}"
  qfile="$(tenant_quadlet_dir "$name")/${container}.container"
  fallback="$(tenant_home "$name")/.config/systemd/user/${container}.service"
  [[ -e "$qfile" || -L "$qfile" ]] && has_quadlet=true
  [[ -e "$fallback" || -L "$fallback" ]] && has_fallback=true
  if [[ "$has_quadlet" == "true" && "$has_fallback" == "true" ]]; then
    say "ERROR: conflicting LunarVision Quadlet and fallback unit for $name" >&2
    return 1
  fi
  if [[ "$has_fallback" == "true" ]]; then
    render_vision_systemd_fallback_unit "$name"
  elif [[ "$has_quadlet" == "true" ]]; then
    render_vision_quadlet "$name"
  elif ! _ctr "$name" inspect "$container" &>/dev/null; then
    return 0
  elif podman_supports_quadlet; then
    render_vision_quadlet "$name"
  else
    render_vision_systemd_fallback_unit "$name"
  fi
}

_remove_voice_systemd_fallback() {
  local name="$1" service="lunarwing-voice-${1}.service" path
  path="$(tenant_home "$name")/.config/systemd/user/$service"
  [[ ! -e "$path" && ! -L "$path" ]] && return 0
  if [[ ! -f "$path" || -L "$path" ]] \
      || ! grep -q '^# Managed by lunarwing-mt-admin: LunarVoice fallback$' "$path"; then
    say "ERROR: refusing to remove unmanaged systemd unit $path" >&2
    return 1
  fi
  _systemctl_user "$name" disable --now "$service" >/dev/null 2>&1 || true
  rm -f -- "$path"
}

# Older Podman (<4.6) has no Quadlet generator, so rootless tenants get a
# supervised user unit around the babysitter helper instead (podman wait).
render_voice_systemd_fallback_unit() {
  local name="$1" container="lunarwing-voice-${1}" unit_dir path tmp runtime_bin babysitter home uid
  unit_dir="$(tenant_home "$name")/.config/systemd/user"
  path="$unit_dir/${container}.service"
  runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  babysitter="${LUNARWING_MT_BABYSITTER_HELPER:-/usr/local/sbin/lunarwing-ctr-babysit}"
  home="$(tenant_home "$name")"
  uid="$(id -u "$name" 2>/dev/null || true)"
  [[ "$CONTAINER_RT" == "podman" && -n "$runtime_bin" && -n "$uid" ]] || return 1
  ensure_babysitter_helper
  [[ -x "$babysitter" ]] || return 1
  if [[ -e "$path" || -L "$path" ]]; then
    if [[ ! -f "$path" || -L "$path" ]] \
        || ! grep -q '^# Managed by lunarwing-mt-admin: LunarVoice fallback$' "$path"; then
      say "ERROR: refusing to overwrite unmanaged systemd unit $path" >&2
      return 1
    fi
  fi
  mkdir -p "$unit_dir"
  tmp="$(mktemp "$unit_dir/.${container}.service.XXXXXX")" || return 1
  cat >"$tmp" <<EOF
# Managed by lunarwing-mt-admin: LunarVoice fallback
[Unit]
Description=LunarVoice ONNX sidecar ($name)
After=network-online.target
Wants=network-online.target
Before=lunarwing-${name}.service
StartLimitIntervalSec=300
StartLimitBurst=10

[Service]
Type=simple
Environment=HOME=${home}
Environment=XDG_RUNTIME_DIR=/run/user/${uid}
ExecStart=${babysitter} ${container}
ExecStop=${runtime_bin} stop --time 30 ${container}
Restart=always
RestartSec=2
TimeoutStopSec=45

[Install]
WantedBy=default.target
EOF
  chown "$name:$name" "$tmp" || { rm -f -- "$tmp"; return 1; }
  chmod 0644 "$tmp" || { rm -f -- "$tmp"; return 1; }
  mv -f -- "$tmp" "$path"
}

# Switching from Quadlet to the fallback shape: retire the generated unit first
# so both never own the same container.
_prepare_voice_systemd_fallback() {
  local name="$1" container="lunarwing-voice-${1}" qfile
  qfile="$(tenant_quadlet_dir "$name")/${container}.container"
  [[ ! -e "$qfile" && ! -L "$qfile" ]] && return 0
  _systemctl_user "$name" stop "${container}.service" >/dev/null 2>&1 || true
  rm -f -- "$qfile"
  _systemctl_user "$name" daemon-reload >/dev/null 2>&1 || return 1
}

_register_voice_systemd_fallback() {
  local name="$1" container="lunarwing-voice-${1}"
  render_voice_systemd_fallback_unit "$name" || return 1
  _systemctl_user "$name" daemon-reload >/dev/null 2>&1 || return 1
  _systemctl_user "$name" reset-failed "${container}.service" >/dev/null 2>&1 || true
  _systemctl_user "$name" enable --now "${container}.service" >/dev/null 2>&1 || return 1
  _wait_tenant_voice_healthy "$name" "$container"
}

render_voice_quadlet() {
  local name="$1" qdir voice_port voice_health_port env_path path tmp
  qdir="$(tenant_quadlet_dir "$name")"
  voice_port="$(ports_get "$name" voice_service)" || return 1
  voice_health_port="$(ports_get "$name" voice_health)" || return 1
  _voice_port_valid "$voice_port" && _voice_port_valid "$voice_health_port" || return 1
  env_path="$(tenant_env_dir "$name")/voice.env"
  [[ -f "$env_path" && ! -L "$env_path" ]] \
    || { say "ERROR: regular LunarVoice env file required: $env_path" >&2; return 1; }
  mkdir -p "$qdir"
  path="$qdir/lunarwing-voice-${name}.container"
  if [[ -e "$path" || -L "$path" ]]; then
    [[ -f "$path" && ! -L "$path" ]] \
      || { say "ERROR: refusing to overwrite unsafe Quadlet path $path" >&2; return 1; }
  fi
  tmp="$(mktemp "$qdir/.lunarwing-voice-${name}.container.XXXXXX")" || return 1
  cat >"$tmp" <<EOF
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
HealthCmd=curl -sf http://127.0.0.1:${VOICE_SIDECAR_HEALTH_PORT}/health && curl -sf http://127.0.0.1:${VOICE_SIDECAR_HEALTH_PORT}/ready || exit 1
HealthInterval=15s
HealthTimeout=5s
HealthRetries=3
HealthStartPeriod=60s
PodmanArgs=--security-opt=no-new-privileges
SecurityLabelDisable=false

[Service]
Restart=on-failure
RestartSec=10
TimeoutStartSec=180

[Install]
WantedBy=default.target
EOF
  chown "$name:$name" "$tmp" || { rm -f -- "$tmp"; return 1; }
  chmod 0644 "$tmp" || { rm -f -- "$tmp"; return 1; }
  mv -f -- "$tmp" "$path"
}

# Refresh an enabled tenant's existing voice supervision shape without starting
# anything (render-units). Disabled tenants get their managed units removed.
# Selection and image availability decide whether units should exist at all;
# start_tenant_voice owns first render + start.
render_voice_systemd_assets() {
  local name="$1" container qfile fallback has_quadlet=false has_fallback=false
  ensure_container_runtime
  container="lunarwing-voice-${name}"
  qfile="$(tenant_quadlet_dir "$name")/${container}.container"
  fallback="$(tenant_home "$name")/.config/systemd/user/${container}.service"
  if ! tenant_voice_enabled "$name"; then
    rm -f -- "$qfile"
    [[ ! -e "$fallback" && ! -L "$fallback" ]] && return 0
    _remove_voice_systemd_fallback "$name"
    return
  fi
  [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]] || return 0
  [[ -e "$qfile" || -L "$qfile" ]] && has_quadlet=true
  [[ -e "$fallback" || -L "$fallback" ]] && has_fallback=true
  if [[ "$has_quadlet" == "true" && "$has_fallback" == "true" ]]; then
    say "ERROR: conflicting LunarVoice Quadlet and fallback unit for $name" >&2
    return 1
  fi
  if [[ "$has_fallback" == "true" ]]; then
    render_voice_systemd_fallback_unit "$name"
  elif [[ "$has_quadlet" == "true" ]]; then
    render_voice_quadlet "$name"
  elif ! _ctr "$name" inspect "$container" &>/dev/null; then
    return 0
  elif podman_supports_quadlet; then
    render_voice_quadlet "$name"
  else
    render_voice_systemd_fallback_unit "$name"
  fi
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

# Render a per-tenant Quadlet .container for an external worker.
# Mirrors the imperative env from start_tenant_<worker>: the WORKER_AUTH_TOKEN ->
# AGENT_AUTH_TOKEN and LLM_API_KEY -> TENSORZERO_API_KEY remap is resolved here and
# inlined as Environment= in a 0600 tenant-owned unit. Kimi uses a separate
# mode-0600 runtime env file so its API key never enters generated unit commands.
# Returns early (no unit) when the worker has no allocated wss port.
render_worker_quadlet() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  case "$worker" in
    pebble|opencode|kimi|opencode2) ;;
    *) die "unsupported external worker '$worker'" ;;
  esac
  local qdir wss_port workspace_dir env_dir tenant_env_path worker_env_path host_health_port
  local runtime_env_path="" runtime_env_hash="" health_path="health"
  qdir="$(tenant_quadlet_dir "$name")"
  wss_port="$(ports_get "$name" "${worker}_wss")"
  host_health_port="$(ports_get "$name" "${worker}_health")"
  [[ -n "$wss_port" ]] || return 0
  env_dir="$(tenant_env_dir "$name")"
  tenant_env_path="$env_dir/lunarwing.env"
  worker_env_path="$env_dir/${worker}.env"
  workspace_dir="$(tenant_lw_root "$name")/${worker}-workspace"
  mkdir -p "$workspace_dir"
  if [[ "$worker" == "kimi" || "$worker" == "opencode" || "$worker" == "opencode2" ]]; then
    workspace_dir="$(prepare_worker_workspace "$name" "$worker")"
  else
    chown "$name:$name" "$workspace_dir"
  fi
  [[ "$worker" == "kimi" || "$worker" == "opencode" || "$worker" == "opencode2" ]] || chmod 777 "$workspace_dir"
  [[ "$worker" != "kimi" ]] || runtime_env_path="$(write_kimi_runtime_env "$name")"
  [[ -z "$runtime_env_path" ]] \
    || runtime_env_hash="$(sha256sum "$runtime_env_path" | cut -d' ' -f1)"
  [[ "$worker" != "kimi" ]] || health_path="ready"
  mkdir -p "$qdir"

  local agent_token tz_key
  # Scoped worker credential (WI#205): the worker is the WS server and this is
  # the only secret it validates. Never the gateway bearer — the value lands in
  # a tenant-readable 0600 unit and in `podman inspect`.
  agent_token="$(resolve_worker_auth_token "$tenant_env_path")"
  tz_key="$(_read_env_value "$tenant_env_path" LLM_API_KEY)"
  # systemd treats % as a unit specifier; escape so a token containing % survives.
  agent_token="${agent_token//%/%%}"
  tz_key="${tz_key//%/%%}"

  # SSH agent socket: the gateway creates an SSH agent server at startup,
  # with its socket at <tenant_home>/lunarwing/run/ssh-agent/ssh-agent.sock
  # (NOT /tmp, because the daemon runs with PrivateTmp=true — a /tmp socket
  # would be invisible to podman containers; and in a dedicated run/ssh-agent/
  # subdir so mounting the directory exposes ONLY the agent — run/ also holds
  # the daemon's REPL control socket lunarwing.sock, which workers must not
  # reach). Non-kimi workers always include the DIRECTORY mount + env var in
  # the quadlet, even if the socket doesn't exist yet at render time — the
  # daemon creates it before the worker needs it (workers only use SSH when a
  # create_job with SSH commands is invoked, well after startup). Workers
  # resolve the socket fresh at connect time, so a daemon restart that binds
  # a new socket inode needs NO worker recreate (the stale-inode fix; the
  # file-mount shape pinned the dead inode until recreation). If SSH is not
  # configured for the tenant, the socket simply won't exist and SSH commands
  # from the worker will fail with a clear "no agent" error. The kimi lane
  # does not mount it at all (WI#206): it is an auto-approved headless
  # executor, and forwarded host credentials are an unacceptable blast radius
  # there.
  local ssh_agent_dir
  ssh_agent_dir="$(tenant_run_dir "$name")/ssh-agent"
  local study_notes_home="" study_notes_state_home="" study_notes_allow_write=""
  if [[ "$worker" == "opencode" || "$worker" == "opencode2" ]]; then
    study_notes_home="$(tenant_study_notes_home "$name")"
    [[ -z "$study_notes_home" ]] || validate_study_notes_home "$study_notes_home"
    if [[ -n "$study_notes_home" ]]; then
      study_notes_state_home="$(tenant_study_notes_state_home "$name")"
      study_notes_allow_write="$(tenant_study_notes_allow_write "$name")"
      if study_notes_write_enabled "$study_notes_allow_write"; then
        [[ -n "$study_notes_state_home" ]] \
          || die "Study Notes write mode requires LW2STUDYMCP_STATE_HOME"
      fi
      [[ -z "$study_notes_state_home" ]] \
        || validate_study_notes_state_home "$study_notes_state_home"
    fi
  fi

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
    if [[ -n "$study_notes_home" ]]; then
      printf 'Volume=%s:%s:ro,z\n' "$study_notes_home" "$STUDY_NOTES_CONTAINER_HOME"
      printf 'Environment=LW2STUDYMCP_HOME=%s\n' "$STUDY_NOTES_CONTAINER_HOME"
      if [[ -n "$study_notes_state_home" ]]; then
        printf 'Volume=%s:%s:ro,z\n' \
          "$study_notes_state_home" "$STUDY_NOTES_CONTAINER_STATE_HOME"
        printf 'Environment=LW2STUDYMCP_STATE_HOME=%s\n' "$STUDY_NOTES_CONTAINER_STATE_HOME"
      fi
    fi
    if [[ "$worker" == "opencode" ]]; then
      printf 'UserNS=%s\n' "$OPENCODE_USERNS"
    elif [[ "$worker" == "kimi" ]]; then
      printf 'UserNS=%s\n' "$KIMI_USERNS"
    elif [[ "$worker" == "opencode2" ]]; then
      printf 'UserNS=%s\n' "$OPENCODE2_USERNS"
    fi
    # WI#206: the kimi lane gets no SSH-agent socket and no privilege
    # escalation — it is an auto-approved headless task executor, and a
    # mounted host agent turns prompt injection into credential use. Other
    # worker lanes keep the socket (their permission model is interactive).
    if [[ "$worker" == "kimi" ]]; then
      # Quadlet has no `SecurityOpt` key: the generator rejects the whole file
      # ("unsupported key 'SecurityOpt' in group 'Container'"), so the unit is
      # never created and the worker is unreachable (connection refused). Use
      # the spelling the other quadlets in this file already use.
      printf 'PodmanArgs=--security-opt=no-new-privileges\n'
    else
      # SSH agent DIRECTORY mount + env (always included — the daemon creates
      # the socket at startup; if SSH isn't configured, the socket won't exist
      # and SSH commands from the worker will fail with a clear "no agent"
      # error). Mounting the directory (not the socket file) means a daemon
      # restart's fresh socket inode is picked up automatically — no worker
      # recreate needed. :ro prevents workers from unlinking or replacing the
      # host socket; :z shares its SELinux label across the worker lanes.
      printf 'Volume=%s:/tmp/ssh-agent:ro,z\n' "$ssh_agent_dir"
      printf 'Environment=SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock\n'
    fi
    # Publish the per-tenant dedicated health port (v8) -> container's 8443, so
    # the host self-heal pipeline can probe the worker directly. Container still
    # listens on HEALTH_PORT=8443 internally (matches image healthcheck).
    [[ -n "$host_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:8443\n' "$host_health_port"
    if [[ "$worker" == "pebble" ]]; then
      printf 'Environment=PEBBLE_MODE=websocket\n'
    elif [[ "$worker" == "opencode" ]]; then
      printf 'Environment=OPENCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    elif [[ "$worker" == "opencode2" ]]; then
      printf 'Environment=OPENCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    elif [[ "$worker" == "kimi" ]]; then
      printf 'Environment=KIMI_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    fi
    [[ "$worker" == "kimi" || -z "$agent_token" ]] \
      || printf 'Environment=AGENT_AUTH_TOKEN=%s\n' "$agent_token"
    [[ "$worker" == "opencode" || "$worker" == "opencode2" ]] && [[ -n "$tz_key" ]] && printf 'Environment=TENSORZERO_API_KEY=%s\n' "$tz_key"
    local oc_model oc_base_url
    oc_model="$(_read_env_value "$tenant_env_path" OPENCODE_MODEL)"
    oc_base_url="$(_read_env_value "$tenant_env_path" OPENCODE_BASE_URL)"
    [[ "$worker" == "opencode" && -n "$oc_model" ]] && printf 'Environment=OPENCODE_MODEL=%s\n' "$oc_model"
    [[ "$worker" == "opencode" && -n "$oc_base_url" ]] && printf 'Environment=OPENCODE_BASE_URL=%s\n' "$oc_base_url"
    local oc2_model oc2_base_url
    oc2_model="$(_read_env_value "$tenant_env_path" OPENCODE2_MODEL)"
    oc2_base_url="$(_read_env_value "$tenant_env_path" OPENCODE2_BASE_URL)"
    [[ "$worker" == "opencode2" && -n "$oc2_model" ]] && printf 'Environment=OPENCODE2_MODEL=%s\n' "$oc2_model"
    [[ "$worker" == "opencode2" && -n "$oc2_base_url" ]] && printf 'Environment=OPENCODE2_BASE_URL=%s\n' "$oc2_base_url"
    if [[ -n "$runtime_env_path" ]]; then
      printf '# RuntimeEnvHash=%s\n' "$runtime_env_hash"
      printf 'EnvironmentFile=%s\n' "$runtime_env_path"
    fi
    # Operator override file (optional). EnvironmentFile= has existed since the
    # Quadlet 4.4 debut, so it is safe at our >= 4.6 floor.
    [[ -f "$worker_env_path" ]] && printf 'EnvironmentFile=%s\n' "$worker_env_path"
    [[ "$worker" == "opencode" || "$worker" == "kimi" || "$worker" == "opencode2" ]] && printf 'Exec=--mode websocket\n'
    cat <<EOF
HealthCmd=curl -sf http://127.0.0.1:${health_port}/${health_path} || exit 1
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
ExecStart=$(command -v tmux) -L weechat-${name} new-session -d -s weechat '$(command -v python3) $RELAY_ENV_EXEC --env-file $env_dir/weechat.env -- $start_helper --weechat-bin $weechat_binary --weechat-home ${weechat_home}'
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
  local relay_exec_prefix=""
  if [[ "$weechat_enabled" == true ]]; then
    prepare_relay_start_barrier "$name" || return 1
    relay_exec_prefix="$(command -v python3) $RELAY_ENV_EXEC --env-file $env_dir/lunarwing.env -- "
  fi

  # Proxy unit (only when enabled). The upstream is the tenant's own
  # TENSORZERO_URL from proxy.env (WI#330: the fleet default used to render a
  # developer-LAN address here, so proxy-enabled tenants on any other host
  # dialed an endpoint that could never answer).
  if tenant_proxy_enabled "$name"; then
  local proxy_upstream
  proxy_upstream="$(read_tenant_env_value "$name" "$env_dir/proxy.env" TENSORZERO_URL)" \
    || die "cannot safely read proxy.env for tenant '$name'"
  [[ -n "$proxy_upstream" ]] \
    || die "proxy enabled for tenant '$name' but $env_dir/proxy.env has no TENSORZERO_URL; re-render with: $0 add-tenant $name --tensorzero-url <url>"
  validate_llm_url "$proxy_upstream" "proxy.env TENSORZERO_URL ($name)"
  cat >"$user_unit_dir/lunarwing-proxy-${name}.service" <<EOF
[Unit]
Description=LunarWing TensorZero proxy ($name)
After=network.target

[Service]
Type=simple
ExecStart=$(command -v python3) $proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero $proxy_upstream
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
ExecStart=${relay_exec_prefix}$(command -v python3) $ws_adapter_path
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
ExecStart=${relay_exec_prefix}$repo/target/${PROFILE}/lunarwing --no-onboard run
Restart=always
RestartSec=5
TimeoutStartSec=60
TimeoutStopSec=30
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
# BUG-005/WI#181: the daemon is capped separately from its transient workloads;
# shell/compile jobs run as transient services under lunarwing-workloads.slice
# (see ic/src/tools/process.rs), so their memory never lands here.
MemoryHigh=$DAEMON_MEMORY_HIGH
MemoryMax=$DAEMON_MEMORY_MAX
TasksMax=$DAEMON_TASKS_MAX

[Install]
WantedBy=default.target
EOF

  # BUG-005/WI#181: transient workload slices. Agent-spawned shell/compile jobs
  # run as transient services under lunarwing-workloads.slice; operator-driven
  # builds (build_tenant) run under lunarwing-build.slice. Slice names are
  # constant across tenants because each tenant has its own systemd user
  # manager.
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

  # Refresh LunarVision's existing supervision shape without starting it. PG and
  # worker Quadlets remain owned by their start functions because selection and
  # image availability determine whether those units should exist.
  render_vision_systemd_assets "$name" || return 1

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
  # enable_list: `systemctl enable` fails on generated/transient units. Each is
  # rendered, started, and health-gated by its owning start_tenant_* function.
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
  local uid state rc stop_failed=false
  uid="$(id -u "$name" 2>/dev/null)" || return 0

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service" "lunarwing-nanocode-${name}.service" "lunarwing-pebble-${name}.service" "lunarwing-opencode-${name}.service" "lunarwing-opencode2-${name}.service" "lunarwing-kimi-${name}.service" "lunarwing-vision-${name}.service" "lunarwing-voice-${name}.service" "lunarwing-pg-${name}.service"; do
    rc=0
    state="$(_systemctl_user "$name" is-active "$svc" 2>/dev/null)" || rc=$?
    case "$state" in
      active|activating|reloading|deactivating) ;;
      inactive|dead|failed|maintenance) continue ;;
      *)
        [[ "$rc" -ne 4 ]] || continue # Absent optional unit.
        say "could not classify $svc before stop" >&2
        stop_failed=true
        continue ;;
    esac
    if _systemctl_user "$name" stop "$svc"; then
      say "stopped $svc"
    else
      say "failed to stop $svc" >&2
      stop_failed=true
    fi
  done
  [[ "$stop_failed" == false ]]
}

uninstall_tenant_systemd() {
  local name="$1"
  local user_unit_dir
  user_unit_dir="$(tenant_home "$name")/.config/systemd/user"

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service"; do
    rm -f "$user_unit_dir/$svc"
  done
  _remove_vision_systemd_fallback "$name" || return 1
  rm -f "$(_hash_dir "$name")/lunarwing-vision-${name}.hash"
  _remove_voice_systemd_fallback "$name" || return 1
  rm -f "$(_hash_dir "$name")/lunarwing-voice-${name}.hash"

  # Quadlet .container units (rootless pg + workers + vision/voice sidecars). Remove the
  # worker containers (their workspace data is bind-mounted in the home); the pg
  # container + named volume are handled by stop_tenant_postgres /
  # reset_tenant_postgres so non-purge removals keep the data for a later re-add.
  local qdir; qdir="$(tenant_quadlet_dir "$name")"
  rm -f "$qdir/lunarwing-pg-${name}.container" \
        "$qdir/lunarwing-nanocode-${name}.container" \
        "$qdir/lunarwing-pebble-${name}.container" \
        "$qdir/lunarwing-opencode-${name}.container" \
        "$qdir/lunarwing-opencode2-${name}.container" \
        "$qdir/lunarwing-kimi-${name}.container" \
        "$qdir/lunarwing-vision-${name}.container" \
        "$qdir/lunarwing-voice-${name}.container"
  if id -u "$name" >/dev/null 2>&1; then
    # Retirement preserves stopped Nanocode containers for rollback and manual
    # recovery. Supported workers remain disposable because their state lives
    # in bind-mounted workspaces.
    for w in pebble opencode kimi opencode2; do
      _ctr "$name" rm -f "lunarwing-${w}-${name}" >/dev/null 2>&1 || true
    done
    _ctr "$name" rm -f "lunarwing-vision-${name}" >/dev/null 2>&1 || true
    _ctr "$name" rm -f "lunarwing-voice-${name}" >/dev/null 2>&1 || true
  fi

  _systemctl_user "$name" daemon-reload 2>/dev/null || true
  say "uninstalled systemd units for $name"
}

cleanup_legacy_weechat_systemd() {
  local name="$1" unit unit_dir unit_path link load_state known=false
  local changed=false
  unit="weechat-${name}.service"
  unit_dir="$(tenant_home "$name")/.config/systemd/user"
  unit_path="$unit_dir/$unit"

  load_state="$(_systemctl_user "$name" show --value -p LoadState "$unit" 2>/dev/null)" \
    || return 1
  [[ "$load_state" == not-found ]] || known=true
  if [[ -e "$unit_path" || -L "$unit_path" ]]; then
    [[ -f "$unit_path" && ! -L "$unit_path" ]] \
      || die "refusing to operate on non-regular legacy unit: $unit_path"
    known=true
  fi
  if [[ "$known" == true ]]; then
    _systemctl_user "$name" disable --now "$unit" || return 1
    changed=true
  fi
  if [[ -e "$unit_path" || -L "$unit_path" ]]; then
    sudo -u "$name" -- rm -- "$unit_path" || return 1
  fi

  for link in "$unit_dir"/*.wants/"$unit" "$unit_dir"/*.requires/"$unit"; do
    [[ -e "$link" || -L "$link" ]] || continue
    [[ -L "$link" ]] || die "refusing to remove non-symlink legacy enablement: $link"
    sudo -u "$name" -- rm -- "$link" || return 1
    changed=true
  done

  if [[ "$changed" == true ]]; then
    _systemctl_user "$name" daemon-reload || return 1
    say "removed legacy systemd user unit $unit for $name"
  else
    say "legacy systemd user unit $unit absent for $name"
  fi
}
