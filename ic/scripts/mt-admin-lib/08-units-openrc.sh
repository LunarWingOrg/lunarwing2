#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 08-units-openrc.sh — part of the lunarwing-mt-admin library.
#
# OpenRC conf.d/init.d rendering (daemon, postgres, vision/voice, weechat),
# env-exec installer, and OpenRC tenant lifecycle.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


_register_vision_openrc() {
  local name="$1" container="lunarwing-vision-${1}" uid home
  render_vision_openrc_unit "$name" || return 1
  _openrc_update add "$container" default >/dev/null 2>&1 || return 1
  _openrc_service "$container" start >/dev/null 2>&1 || return 1
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  uid="$(id -u "$name")" || return 1
  home="$(tenant_home "$name")"
  render_container_babysitter_unit "$name" vision "$container" "$uid" "$home" || return 1
  _openrc_update add "${container}-sup" default >/dev/null 2>&1 || return 1
  _openrc_service "${container}-sup" start >/dev/null 2>&1 || return 1
}

render_vision_openrc_unit() {
  local name="$1" vision_port runtime_bin="" container uid home unit_path
  ensure_container_runtime
  vision_port="$(ports_get "$name" vision_service)" || return 1
  _vision_port_valid "$vision_port" || return 1
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  container="lunarwing-vision-${name}"
  uid="$(id -u "$name" 2>/dev/null || true)"
  home="$(tenant_home "$name")"
  unit_path="$OPENRC_INITD_DIR/$container"
  mkdir -p "$OPENRC_INITD_DIR"

  cat >"$unit_path" <<INITEOF
#!/sbin/openrc-run

description="LunarWing vision sidecar ($name)"

: "\${vis_runtime:=$runtime_bin}"
: "\${vis_container:=$container}"
: "\${vis_rootless:=$MT_ROOTLESS}"
: "\${vis_user:=$name}"
: "\${vis_home:=$home}"
: "\${vis_uid:=$uid}"
: "\${vis_health_internal_port:=$VISION_SIDECAR_HEALTH_PORT}"
: "\${vis_wait:=30}"

depend() {
    need net localmount
    after firewall
    before lunarwing-${name}
}

_vis() {
    if [ "\${vis_rootless}" = "true" ]; then
        sudo -u "\${vis_user}" env HOME="\${vis_home}" XDG_RUNTIME_DIR="/run/user/\${vis_uid}" "\${vis_runtime}" "\$@"
    else
        "\${vis_runtime}" "\$@"
    fi
}

_vis_healthy() {
    [ "\$(_vis inspect -f '{{.State.Running}}' "\${vis_container}" 2>/dev/null)" = "true" ] || return 1
    _vis exec "\${vis_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${vis_health_internal_port}/health" 2>/dev/null
}

start() {
    [ -n "\${vis_runtime}" ] && [ -x "\${vis_runtime}" ] || { eend 1 "container runtime unavailable"; return 1; }
    ebegin "Starting vision sidecar (\${vis_container})"
    if [ "\${vis_rootless}" = "true" ]; then
        [ -n "\${vis_uid}" ] || { eend 1 "tenant uid unavailable"; return 1; }
        checkpath -d -m 0700 -o "\${vis_user}:\${vis_user}" "/run/user/\${vis_uid}"
    fi
    _vis start "\${vis_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _vis_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${vis_wait}" ] || { eend 1 "vision sidecar not healthy after \${vis_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${vis_runtime}" ] && [ -x "\${vis_runtime}" ] || return 0
    ebegin "Stopping vision sidecar (\${vis_container})"
    _vis stop --time 30 "\${vis_container}" >/dev/null 2>&1
    eend \$?
}

status() {
    if _vis_healthy; then
        einfo "\${vis_container}: started"; return 0
    fi
    einfo "\${vis_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "$unit_path"
}

remove_vision_openrc_assets() {
  local name="$1" container="lunarwing-vision-${1}" path
  for path in "$OPENRC_INITD_DIR/${container}-sup" "$OPENRC_INITD_DIR/$container"; do
    [[ ! -e "$path" && ! -L "$path" ]] && continue
    [[ -f "$path" && ! -L "$path" ]] \
      || { say "ERROR: refusing to remove unsafe LunarVision OpenRC unit $path" >&2; return 1; }
    _openrc_service "${path##*/}" stop >/dev/null 2>&1 || true
    _openrc_update del "${path##*/}" default >/dev/null 2>&1 || true
    rm -f -- "$path"
  done
  rm -f -- "$(_hash_dir "$name")/${container}.hash"
}

render_vision_openrc_assets() {
  local name="$1" uid home container="lunarwing-vision-${1}" direct supervisor
  if tenant_vision_external "$name"; then
    remove_vision_openrc_assets "$name"
    return
  fi
  direct="$OPENRC_INITD_DIR/$container"
  supervisor="$OPENRC_INITD_DIR/${container}-sup"
  if [[ ! -e "$direct" && ! -L "$direct" && ! -e "$supervisor" && ! -L "$supervisor" ]] \
      && ! _ctr "$name" inspect "$container" &>/dev/null; then
    return 0
  fi
  render_vision_openrc_unit "$name" || return 1
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  uid="$(id -u "$name")" || return 1
  home="$(tenant_home "$name")"
  render_container_babysitter_unit "$name" vision "$container" "$uid" "$home"
}

_register_voice_openrc() {
  local name="$1" container="lunarwing-voice-${1}" uid home
  render_voice_openrc_unit "$name" || return 1
  _openrc_update add "$container" default >/dev/null 2>&1 || return 1
  _openrc_service "$container" start >/dev/null 2>&1 || return 1
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  uid="$(id -u "$name")" || return 1
  home="$(tenant_home "$name")"
  render_container_babysitter_unit "$name" voice "$container" "$uid" "$home" || return 1
  _openrc_update add "${container}-sup" default >/dev/null 2>&1 || return 1
  _openrc_service "${container}-sup" start >/dev/null 2>&1 || return 1
}

render_voice_openrc_unit() {
  local name="$1" runtime_bin="" container uid home unit_path
  ensure_container_runtime
  container="lunarwing-voice-${name}"
  uid="$(id -u "$name" 2>/dev/null || true)"
  home="$(tenant_home "$name")"
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  unit_path="$OPENRC_INITD_DIR/${container}"
  mkdir -p "$OPENRC_INITD_DIR"
  cat >"$unit_path" <<INITEOF
#!/sbin/openrc-run

description="LunarVoice ONNX sidecar ($name)"

: "\${voice_runtime:=$runtime_bin}"
: "\${voice_container:=$container}"
: "\${voice_rootless:=$MT_ROOTLESS}"
: "\${voice_user:=$name}"
: "\${voice_home:=$home}"
: "\${voice_uid:=$uid}"
: "\${voice_health_internal_port:=$VOICE_SIDECAR_HEALTH_PORT}"
: "\${voice_wait:=60}"

depend() {
    need net localmount
    after firewall
    before lunarwing-${name}
}

_voice() {
    if [ "\${voice_rootless}" = "true" ]; then
        sudo -u "\${voice_user}" env HOME="\${voice_home}" XDG_RUNTIME_DIR="/run/user/\${voice_uid}" "\${voice_runtime}" "\$@"
    else
        "\${voice_runtime}" "\$@"
    fi
}

_voice_healthy() {
    [ "\$(_voice inspect -f '{{.State.Running}}' "\${voice_container}" 2>/dev/null)" = "true" ] || return 1
    _voice exec "\${voice_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${voice_health_internal_port}/health" 2>/dev/null \
        && _voice exec "\${voice_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${voice_health_internal_port}/ready" 2>/dev/null
}

start() {
    [ -n "\${voice_runtime}" ] && [ -x "\${voice_runtime}" ] || { eend 1 "container runtime unavailable"; return 1; }
    ebegin "Starting LunarVoice (\${voice_container})"
    if [ "\${voice_rootless}" = "true" ]; then
        [ -n "\${voice_uid}" ] || { eend 1 "tenant uid unavailable"; return 1; }
        checkpath -d -m 0700 -o "\${voice_user}:\${voice_user}" "/run/user/\${voice_uid}"
    fi
    _voice start "\${voice_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _voice_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${voice_wait}" ] || { eend 1 "voice sidecar not healthy after \${voice_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${voice_runtime}" ] && [ -x "\${voice_runtime}" ] || return 0
    ebegin "Stopping LunarVoice (\${voice_container})"
    _voice stop --time 30 "\${voice_container}" >/dev/null 2>&1
    eend \$?
}

status() {
    if _voice_healthy; then
        einfo "\${voice_container}: started"; return 0
    fi
    einfo "\${voice_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "$unit_path"
}

render_voice_openrc_assets() {
  local name="$1" uid home container="lunarwing-voice-${1}" direct supervisor
  direct="$OPENRC_INITD_DIR/$container"
  supervisor="$OPENRC_INITD_DIR/${container}-sup"
  if ! tenant_voice_enabled "$name"; then
    _openrc_update del "${container}-sup" default >/dev/null 2>&1 || true
    _openrc_update del "$container" default >/dev/null 2>&1 || true
    rm -f -- "$direct" "$supervisor"
    return 0
  fi
  if [[ ! -e "$direct" && ! -L "$direct" && ! -e "$supervisor" && ! -L "$supervisor" ]] \
      && ! _ctr "$name" inspect "$container" &>/dev/null; then
    return 0
  fi
  render_voice_openrc_unit "$name" || return 1
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  uid="$(id -u "$name")" || return 1
  home="$(tenant_home "$name")"
  render_container_babysitter_unit "$name" voice "$container" "$uid" "$home"
}

# Render the WeeChat OpenRC init script into <out_file>.
_render_weechat_openrc_unit() {  # <tenant> <out_file>
  local name="$1" out_file="$2"
  local weechat_home env_dir run_dir log_dir
  weechat_home="$(tenant_weechat_home "$name")"
  env_dir="$(tenant_env_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  local start_helper stop_helper
  start_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-start.sh"
  stop_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-stop.sh"
  cat >"$out_file" <<INITEOF
#!/sbin/openrc-run

description="WeeChat IRC client ($name)"

: "\${weechat_user:=$name}"
: "\${weechat_group:=$name}"
: "\${weechat_home:=$weechat_home}"
: "\${weechat_pidfile:=$run_dir/weechat.pid}"
: "\${weechat_runtime_dir:=$run_dir}"
: "\${weechat_log_dir:=$log_dir}"
: "\${weechat_output_log:=\${weechat_log_dir}/weechat.log}"
: "\${weechat_error_log:=\${weechat_log_dir}/weechat.err}"
: "\${weechat_retry:=SIGTERM/30/KILL/5}"
: "\${weechat_env_file:=$env_dir/weechat.env}"
: "\${weechat_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${weechat_command:=$(command -v tmux)}"
: "\${weechat_binary:=$(command -v weechat)}"
: "\${weechat_start_helper:=$start_helper}"
: "\${weechat_stop_helper:=$stop_helper}"

command="\${weechat_openrc_env_exec}"
command_args="--env-file \${weechat_env_file} -- \${weechat_command} -L weechat-${name} new-session -d -s weechat '$(command -v python3) $RELAY_ENV_EXEC --env-file \${weechat_env_file} -- \${weechat_start_helper} --weechat-bin \${weechat_binary} --weechat-home \${weechat_home}'"
command_user="\${weechat_user}:\${weechat_group}"
pidfile="\${weechat_pidfile}"
required_files="\${weechat_openrc_env_exec} \${weechat_command} \${weechat_binary} \${weechat_start_helper} \${weechat_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-weechat-adapter-${name} lunarwing-${name}
}

start() {
    ebegin "Starting WeeChat ($name)"
    checkpath -d -m 0750 -o "\${weechat_user}:\${weechat_group}" "\${weechat_home}"
    checkpath -d -m 0750 -o "\${weechat_user}:\${weechat_group}" "\${weechat_runtime_dir}"
    start-stop-daemon --start --background --user "\${weechat_user}" \\
        --exec "\${weechat_openrc_env_exec}" -- \\
        --env-file "\${weechat_env_file}" -- "\${weechat_command}" \\
        -L weechat-${name} new-session -d -s weechat \
        "$(command -v python3) $RELAY_ENV_EXEC --env-file \${weechat_env_file} -- \${weechat_start_helper} --weechat-bin \${weechat_binary} --weechat-home \${weechat_home}"
    start_rc=\$?
    if [ "\${start_rc}" -eq 0 ]; then
        # tmux new-session -d returns immediately and the real work runs in the
        # detached tmux server, leaving OpenRC with no process to track (which
        # surfaces as "status: crashed"). Record the tmux server PID so the
        # pidfile, status, and health checks see the real process.
        tmux_pid=""
        i=0
        while [ "\$i" -lt 50 ]; do
            tmux_pid="\$(start-stop-daemon --start \\
                --chuid "\${weechat_user}:\${weechat_group}" \\
                --exec "\${weechat_command}" -- \\
                -L "weechat-${name}" display-message -p '#{pid}' 2>/dev/null)"
            [ -n "\$tmux_pid" ] && break
            i=\$((i + 1))
            sleep 0.1
        done
        if [ -n "\$tmux_pid" ]; then
            printf '%s\n' "\$tmux_pid" >"\${weechat_pidfile}"
            chown "\${weechat_user}:\${weechat_group}" "\${weechat_pidfile}" 2>/dev/null || true
        fi
    fi
    eend "\${start_rc}"
}

status() {
    # Probe the real tmux session instead of only the OpenRC state file, so a
    # live session never reports as crashed.
    if start-stop-daemon --start \\
        --chuid "\${weechat_user}:\${weechat_group}" \\
        --exec "\${weechat_command}" -- \\
        -L "weechat-${name}" has-session -t weechat 2>/dev/null; then
        einfo "WeeChat ($name) session is up"
        return 0
    fi
    einfo "WeeChat ($name) session is not running"
    return 1
}

stop() {
    ebegin "Stopping WeeChat ($name)"
    # Graceful: /upgrade -quit via FIFO saves buffers before exit; falls back
    # to tmux kill-session if the FIFO is unavailable or times out. Must run
    # under the tenant UID because tmux sockets are UID-scoped. --chuid drops
    # privileges before exec; --user only filters by owner and would leave the
    # helper running as root, unable to reach the tenant's tmux socket.
    if [ -x "\${weechat_stop_helper}" ]; then
        (
            unset RC_SVCNAME
            start-stop-daemon --start \\
                --chuid "\${weechat_user}:\${weechat_group}" \\
                --exec "\${weechat_stop_helper}" -- \\
                --weechat-home "\${weechat_home}" \\
                --tmux-socket "weechat-${name}" \\
                --session weechat \\
                --timeout 20
        )
        stop_rc=\$?
    else
        (
            unset RC_SVCNAME
            start-stop-daemon --start \\
                --chuid "\${weechat_user}:\${weechat_group}" \\
                --exec "\${weechat_command}" -- \\
                -L "weechat-${name}" kill-session -t weechat
        ) >/dev/null 2>&1
        stop_rc=\$?
    fi
    [ "\${stop_rc}" -eq 0 ] && rm -f "\${weechat_pidfile}"
    eend "\${stop_rc}"
    return "\${stop_rc}"
}
INITEOF
  chmod 0755 "$out_file"
}

# ── OpenRC service units ─────────────────────────────────────────────────────

# The post-WI#376 proxy unit contract: one fixed flag line carrying no tenant
# data. Shared by the preflight guard and the renderer, which archives only
# installed units that fail this check. Byte-identical copies of this contract
# run inside the root health cron (WI#410): ic-infrastructure-health-check/
# health-openrc.sh and lunarwing-self-heal.sh. Keep the three in sync.
openrc_proxy_unit_is_safe() {
  local path="$1"
  [[ -f "$path" && ! -L "$path" ]] || return 1
  grep -Eq \
    '^: "\$\{proxy_args:=[/a-zA-Z0-9_.-]+/lunarwing-proxy[.]py --port [0-9]+ --bind 127[.]0[.]0[.]1 --tensorzero-env\}"$' "$path"
}

# A dependency-cache rebuild can source every init script, even for a command
# addressing a different tenant. Inspect files as data before invoking OpenRC.
require_safe_openrc_proxy_units() {
  local path
  for path in "$OPENRC_INITD_DIR"/lunarwing-proxy-*; do
    [[ -e "$path" || -L "$path" ]] || continue
    if ! openrc_proxy_unit_is_safe "$path"; then
      say "refusing OpenRC operation: unsafe legacy proxy unit $path; run: $0 render-units --all-proxies" >&2
      return 125
    fi
  done
}

# Refuse before a dispatcher handler can perform writes or interpret a refused
# probe as an inactive service. Keep help and the file-only migration paths open.
openrc_dispatch_preflight() {
  case "$1" in
    help|--help|-h|upgrade-tenant) return 0 ;;
    render-units) [[ $# -eq 2 && "$2" == --all-proxies ]] && return 0 ;;
  esac
  # Detection failure must not become a false "migration required" refusal
  # (FL-1): only a positively detected systemd host skips the file-only guard.
  # On failure the command's own ensure_init_system reports the accurate error.
  ensure_init_system 2>/dev/null || true
  [[ "$INIT_SYSTEM" == systemd ]] || require_safe_openrc_proxy_units
}

_openrc_service() {
  # All mt-admin rc-service/rc-update calls go through these guards, including
  # worker, sidecar and cron operations which can also trigger a deptree rebuild.
  # 125 is a refused operation, never an OpenRC service state.
  require_safe_openrc_proxy_units || return 125
  rc-service "$@"
}

_openrc_update() {
  require_safe_openrc_proxy_units || return 125
  rc-update "$@"
}

# File-only fleet migration. Do not call the full tenant renderer here: its
# retirement/sidecar helpers can invoke OpenRC before other tenants are repaired.
# Include installed-but-disabled proxies; refuse orphaned or unsafe paths so
# they cannot be overlooked by a registry-only sweep. No tenant env is read.
render_all_openrc_proxy_units() {
  ensure_init_system
  [[ "$INIT_SYSTEM" == openrc ]] || die "render-units --all-proxies requires OpenRC"
  ports_registry_require_readonly
  local names name path
  names="$(all_tenant_names)" || return 1
  local -A selected=()
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    [[ "$name" == "$(sanitize_name "$name")" ]] || die "invalid tenant name in registry"
    if tenant_proxy_enabled "$name"; then selected["$name"]=1; fi
  done <<<"$names"
  for path in "$OPENRC_INITD_DIR"/lunarwing-proxy-*; do
    [[ -e "$path" || -L "$path" ]] || continue
    [[ -f "$path" && ! -L "$path" ]] || die "unsafe installed proxy unit: $path"
    name="${path##*/lunarwing-proxy-}"
    [[ -n "$name" && "$name" == "$(sanitize_name "$name")" ]] \
      && tenant_exists_in_registry "$name" \
      || die "unregistered proxy unit $path; inspect and move it outside init.d before retrying (do not invoke OpenRC)"
    selected["$name"]=1
  done
  install_openrc_env_exec || return 1
  for name in "${!selected[@]}"; do
    render_proxy_openrc_unit "$name" || return 1
  done
  require_safe_openrc_proxy_units || return 1
  say "OpenRC proxy units neutralized fleet-wide (file writes only; services not restarted)."
}

# Validate the configured archive location without writing anything. The
# renderer calls this even when the installed unit needs no archive copy: an
# archive dir inside init.d could hand OpenRC an unparseable copy at the next
# dependency scan, so a misconfigured archive always fails the render. Every
# refusal names the reason and LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR (EQ-5).
openrc_proxy_backup_dir_validate() {
  local dir init_dir
  if ! darkirc_path_components_safe "$OPENRC_PROXY_BACKUP_DIR"; then
    say "unsafe archive path: symlinked component in LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR=$OPENRC_PROXY_BACKUP_DIR" >&2
    return 1
  fi
  dir="$(realpath -m -- "$OPENRC_PROXY_BACKUP_DIR")" \
    || { say "unsafe archive path: cannot resolve LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR=$OPENRC_PROXY_BACKUP_DIR" >&2; return 1; }
  init_dir="$(realpath -m -- "$OPENRC_INITD_DIR")" || return 1
  if [[ "$dir" == "$init_dir" || "$dir" == "$init_dir/"* ]]; then
    say "unsafe archive path: LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR=$OPENRC_PROXY_BACKUP_DIR must be outside $OPENRC_INITD_DIR" >&2
    return 1
  fi
  if [[ -e "$dir" ]]; then
    if [[ ! -d "$dir" || -L "$dir" || "$(stat -c '%u:%a' -- "$dir")" != "$EUID:700" ]]; then
      say "unsafe archive dir $dir (LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR): must be a non-symlink dir owned by EUID $EUID with mode 0700" >&2
      return 1
    fi
  fi
}

# Preserve the installed bytes before replacement, including suspicious URLs.
# CLI writers require root; EUID also permits isolated non-root test fixtures.
# The archive is private, outside init.d, non-executable, and never sourced.
backup_openrc_proxy_unit() {
  local path="$1" dir backup
  [[ -e "$path" || -L "$path" ]] || return 0
  if [[ ! -f "$path" || -L "$path" ]]; then
    say "cannot safely back up $path: not a regular file (symlink?); installed unit was not replaced" >&2
    return 1
  fi
  openrc_proxy_backup_dir_validate || return 1
  dir="$(realpath -m -- "$OPENRC_PROXY_BACKUP_DIR")" || return 1
  if [[ ! -e "$dir" ]]; then
    install -d -m 0700 -- "$dir" \
      || { say "cannot safely back up $path: cannot create backup dir $dir; installed unit was not replaced" >&2; return 1; }
  fi
  backup="$(mktemp "$dir/${path##*/}.XXXXXX")" \
    || { say "cannot safely back up $path: mktemp failed in $dir; installed unit was not replaced" >&2; return 1; }
  if ! cat -- "$path" >"$backup" || ! chmod 0600 "$backup"; then
    rm -f -- "$backup"
    say "cannot safely back up $path: copy to $dir failed; installed unit was not replaced" >&2
    return 1
  fi
  say "preserved installed proxy unit: $backup"
}

# File-only proxy renderer shared by normal rendering and the fleet migration. The
# atomic replacement prevents dependency scanning from seeing a partial script.
render_proxy_openrc_unit() {
  local name="$1" proxy_port proxy_bin env_dir run_dir log_dir path tmp
  proxy_port="$(ports_get "$name" proxy)" || return 1
  [[ "$proxy_port" =~ ^[0-9]+$ ]] || die "invalid proxy port for $name"
  proxy_bin="$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"
  env_dir="$(tenant_env_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  mkdir -p "$OPENRC_INITD_DIR" || return 1
  path="$OPENRC_INITD_DIR/lunarwing-proxy-${name}"
  [[ ! -L "$path" && ( ! -e "$path" || -f "$path" ) ]] || die "unsafe installed proxy unit: $path"
  tmp="$(mktemp "$OPENRC_INITD_DIR/.lunarwing-proxy-${name}.XXXXXX")" || return 1
  if ! cat >"$tmp" <<INITEOF
#!/sbin/openrc-run

description="LunarWing TensorZero proxy ($name)"

: "\${proxy_command:=$(command -v python3)}"
: "\${proxy_args:=$proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero-env}"
: "\${proxy_user:=$name}"
: "\${proxy_group:=$name}"
: "\${proxy_pidfile:=$run_dir/proxy.pid}"
: "\${proxy_runtime_dir:=$run_dir}"
: "\${proxy_log_dir:=$log_dir}"
: "\${proxy_output_log:=\${proxy_log_dir}/proxy.log}"
: "\${proxy_error_log:=\${proxy_log_dir}/proxy.err}"
: "\${proxy_env_file:=$env_dir/proxy.env}"
: "\${proxy_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${proxy_umask:=0077}"
: "\${proxy_respawn_delay:=5}"
: "\${proxy_respawn_max:=5}"
: "\${proxy_respawn_period:=60}"
: "\${proxy_retry:=SIGTERM/30/KILL/5}"

command="\${proxy_openrc_env_exec}"
command_args="--env-file \${proxy_env_file} -- \${proxy_command} \${proxy_args}"
command_user="\${proxy_user}:\${proxy_group}"
pidfile="\${proxy_pidfile}"
supervisor="supervise-daemon"
retry="\${proxy_retry}"
respawn_delay="\${proxy_respawn_delay}"
respawn_max="\${proxy_respawn_max}"
respawn_period="\${proxy_respawn_period}"
output_log="\${proxy_output_log}"
error_log="\${proxy_error_log}"
required_files="\${proxy_openrc_env_exec} \${proxy_command} \${proxy_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${proxy_user}:\${proxy_group}" "\${proxy_runtime_dir}"
    checkpath -d -m 0750 -o "\${proxy_user}:\${proxy_group}" "\${proxy_log_dir}"
    checkpath -f -m 0640 -o "\${proxy_user}:\${proxy_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${proxy_user}:\${proxy_group}" "\${error_log}"
    umask "\${proxy_umask}"
}
INITEOF
  then
    rm -f -- "$tmp"
    return 1
  fi
  # The archive location is validated on every render, even when this render
  # writes no copy: an archive dir inside init.d could hand OpenRC an
  # unparseable copy at the next dependency scan, so misconfiguration refuses.
  if ! openrc_proxy_backup_dir_validate; then
    rm -f -- "$tmp"
    return 1
  fi
  # Archive only units that fail the safe-unit contract (legacy or damaged
  # installs whose bytes may carry a payload). Re-rendering an already-safe
  # unit must not grow the archive on every upgrade or --all-proxies sweep
  # (EQ-4); safe bytes are reproducible from the renderer, so nothing is lost.
  if [[ -e "$path" || -L "$path" ]] && ! openrc_proxy_unit_is_safe "$path"; then
    backup_openrc_proxy_unit "$path" || {
      rm -f -- "$tmp"
      return 1
    }
  fi
  if ! chmod 0755 "$tmp" || ! mv -T -- "$tmp" "$path"; then
    rm -f -- "$tmp"
    return 1
  fi
}

validate_openrc_proxy_env() {
  local name="$1" env_file proxy_upstream
  env_file="$(tenant_env_dir "$name")/proxy.env"
  proxy_upstream="$(read_tenant_env_value "$name" "$env_file" TENSORZERO_URL)" \
    || die "cannot safely read proxy.env for tenant '$name'"
  [[ -n "$proxy_upstream" ]] \
    || die "proxy enabled for tenant '$name' but $env_file has no TENSORZERO_URL; configure it before rendering or upgrading"
  validate_llm_url "$proxy_upstream" "proxy.env TENSORZERO_URL ($name)"
}

install_openrc_env_exec() {
  local install_dir owner group mode links
  [[ -f "$OPENRC_ENV_EXEC_SRC" && ! -L "$OPENRC_ENV_EXEC_SRC" ]] \
    || die "OpenRC tenant env launcher is missing: $OPENRC_ENV_EXEC_SRC"

  install_dir="$(dirname "$OPENRC_ENV_EXEC")"
  install -d -o root -g root -m 0755 "$install_dir" \
    || die "failed to prepare OpenRC tenant env launcher directory"
  [[ -d "$install_dir" && ! -L "$install_dir" ]] \
    || die "unsafe OpenRC tenant env launcher directory: $install_dir"
  owner="$(stat -c '%u' "$install_dir" 2>/dev/null || true)"
  group="$(stat -c '%g' "$install_dir" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$install_dir" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$group" == 0 && "$mode" == 755 ]] \
    || die "OpenRC tenant env launcher directory must be root:root mode 0755"

  install -o root -g root -m 0755 "$OPENRC_ENV_EXEC_SRC" "$OPENRC_ENV_EXEC" \
    || die "failed to install OpenRC tenant env launcher"
  [[ -f "$OPENRC_ENV_EXEC" && ! -L "$OPENRC_ENV_EXEC" && -x "$OPENRC_ENV_EXEC" ]] \
    || die "unsafe OpenRC tenant env launcher: $OPENRC_ENV_EXEC"
  owner="$(stat -c '%u' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  group="$(stat -c '%g' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  links="$(stat -c '%h' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$group" == 0 && "$mode" == 755 && "$links" == 1 ]] \
    || die "OpenRC tenant env launcher must be root:root, single-link mode 0755"
}

_render_openrc_lunarwing_confd() {
  local name="$1" dependencies="$2"
  local path begin end header tmp begin_count end_count first second
  local links mode owner group dir_mode dir_owner dir_group
  path="$OPENRC_CONFD_DIR/lunarwing-$name"
  begin="# BEGIN lunarwing-mt-admin managed dependencies"
  end="# END lunarwing-mt-admin managed dependencies"
  header="# Auto-generated by lunarwing-mt-admin.sh for tenant: $name"

  [[ -d "$OPENRC_CONFD_DIR" && ! -L "$OPENRC_CONFD_DIR" ]] \
    || die "unsafe OpenRC conf.d directory: $OPENRC_CONFD_DIR"
  dir_mode="$(stat -c %a "$OPENRC_CONFD_DIR" 2>/dev/null || true)"
  dir_owner="$(stat -c %u "$OPENRC_CONFD_DIR" 2>/dev/null || true)"
  dir_group="$(stat -c %g "$OPENRC_CONFD_DIR" 2>/dev/null || true)"
  [[ "$dir_mode" =~ ^[0-7]+$ && "$dir_owner" =~ ^[0-9]+$ && "$dir_group" =~ ^[0-9]+$ ]] \
    || die "cannot validate OpenRC conf.d directory metadata: $OPENRC_CONFD_DIR"
  (( (8#$dir_mode & 022) == 0 )) || die "unsafe writable OpenRC conf.d directory: $OPENRC_CONFD_DIR"
  [[ ! -L "$path" && ( ! -e "$path" || -f "$path" ) ]] \
    || die "refusing to replace non-regular OpenRC conf.d file: $path"
  if [[ -e "$path" ]]; then
    links="$(stat -c %h "$path" 2>/dev/null || true)"
    mode="$(stat -c %a "$path" 2>/dev/null || true)"
    owner="$(stat -c %u "$path" 2>/dev/null || true)"
    group="$(stat -c %g "$path" 2>/dev/null || true)"
    [[ "$links" == 1 && "$mode" =~ ^[0-7]+$ && "$group" =~ ^[0-9]+$ \
      && "$owner" == "$dir_owner" ]] \
      || die "unsafe OpenRC conf.d ownership, link count, or mode: $path"
    (( (8#$mode & 022) == 0 )) || die "unsafe writable OpenRC conf.d mode: $path"
  fi

  tmp="$(mktemp "$OPENRC_CONFD_DIR/.lunarwing-$name.confd.XXXXXX")"
  if [[ ! -e "$path" ]]; then
    cat >"$tmp" <<CONFD
$header
$begin
lunarwing_rc_need="$dependencies"
$end

# BUG-005 / work item #54: agent-spawned shell/compile workloads are isolated
# into their own process group and reaped on timeout/cancel by the daemon
# itself (see ic/src/tools/process.rs), so runaway trees cannot leak on any
# init system. Hard cgroup v2 ceilings (memory.high/memory.max/cpu.max/
# pids.max) are composed into the init script's rc_cgroup_settings; OpenRC has
# no transient scopes, so the daemon and its workloads share one cgroup and
# the defaults use the workload outer bounds. Tune per tenant here, e.g.:
#lunarwing_cgroup_memory_max="12G"
#lunarwing_cgroup_cpu_max="400000 100000"
#lunarwing_cgroup_pids_max="4096"
#lunarwing_cgroup_limits_enabled="false"   # emergency opt-out
# K/M/G/T memory suffixes use powers of 1024 and are converted to bytes before
# OpenRC writes cgroup files. On cgroup v2 hosts, startup fails if OpenRC skips
# a configured knob; use the explicit opt-out only for emergency recovery.
# Optional process-level fallback cap (works on any host):
#rc_ulimit="-v 8388608"
CONFD
  else
    begin_count="$(grep -Fxc -- "$begin" "$path" 2>/dev/null || true)"
    end_count="$(grep -Fxc -- "$end" "$path" 2>/dev/null || true)"
    if [[ "$begin_count" == 1 && "$end_count" == 1 ]]; then
      if ! awk -v begin="$begin" -v end="$end" -v dependencies="$dependencies" '
        $0 == begin {
          if (seen || inside) exit 42
          print begin
          print "lunarwing_rc_need=\"" dependencies "\""
          print end
          seen = 1
          inside = 1
          next
        }
        $0 == end {
          if (!inside) exit 43
          inside = 0
          next
        }
        inside { next }
        /^lunarwing_rc_need=/ { exit 44 }
        { print }
        END { if (inside || seen != 1) exit 45 }
      ' "$path" >"$tmp"; then
        rm -f -- "$tmp"
        die "malformed managed dependency block in $path"
      fi
    elif [[ "$begin_count" == 0 && "$end_count" == 0 ]]; then
      first="$(sed -n '1p' "$path")"
      second="$(sed -n '2p' "$path")"
      if [[ "$first" == "$header" && "$second" == lunarwing_rc_need=* ]]; then
        {
          printf '%s\n%s\nlunarwing_rc_need="%s"\n%s\n' \
            "$header" "$begin" "$dependencies" "$end"
          sed -n '3,$p' "$path"
        } >"$tmp"
      else
        if grep -q '^lunarwing_rc_need=' "$path"; then
          rm -f -- "$tmp"
          die "refusing to replace unmanaged lunarwing_rc_need in $path"
        fi
        {
          printf '%s\nlunarwing_rc_need="%s"\n%s\n\n' \
            "$begin" "$dependencies" "$end"
          cat "$path"
        } >"$tmp"
      fi
    else
      rm -f -- "$tmp"
      die "malformed managed dependency block in $path"
    fi
  fi

  if [[ -e "$path" ]]; then
    if ! chmod --reference="$path" "$tmp" || ! chown --reference="$path" "$tmp"; then
      rm -f -- "$tmp"
      die "failed to preserve OpenRC conf.d metadata: $path"
    fi
  else
    if ! chmod 0644 "$tmp" || ! chown --reference="$OPENRC_CONFD_DIR" "$tmp"; then
      rm -f -- "$tmp"
      die "failed to set OpenRC conf.d metadata: $path"
    fi
  fi
  mv -T -- "$tmp" "$path" || {
    rm -f -- "$tmp"
    die "failed to install OpenRC conf.d file: $path"
  }
}

render_tenant_openrc_units() {
  local name="$1"
  local repo env_dir state_dir log_dir run_dir
  repo="$(tenant_repo "$name")"
  env_dir="$(tenant_env_dir "$name")"
  state_dir="$(tenant_state_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  install_openrc_env_exec
  mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR"

  local bridge_port weechat_port
  bridge_port="$(ports_get "$name" bridge)"
  weechat_port="$(ports_get "$name" weechat)"

  local ws_adapter_path
  ws_adapter_path="$(tenant_lw_root "$name")/lunarwing_weechat_wss/weechat_relay/ws_adapter.py"
  local ws_adapter_dir
  ws_adapter_dir="$(dirname "$ws_adapter_path")"
  local weechat_enabled=false
  tenant_weechat_enabled "$name" && weechat_enabled=true
  local relay_exec_prefix=""
  if [[ "$weechat_enabled" == true ]]; then
    prepare_relay_start_barrier "$name" || return 1
    relay_exec_prefix="$(command -v python3) $RELAY_ENV_EXEC --env-file $env_dir/lunarwing.env -- "
  fi

  local darkirc_rc_after=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_rc_after=" lunarwing-darkirc-adapter-${name}"
  fi

  local proxy_rc_after=""
  if tenant_proxy_enabled "$name"; then
    proxy_rc_after=" lunarwing-proxy-${name}"
  fi

  local weechat_rc_after=""
  if [[ "$weechat_enabled" == "true" ]]; then
    weechat_rc_after=" lunarwing-weechat-${name} lunarwing-weechat-adapter-${name}"
  fi

  local weechat_home
  weechat_home="$(tenant_home "$name")/.config/weechat"

  # Resolve the container runtime path + tenant identity so the dedicated
  # Postgres init service can bring the container up (rootless: as the tenant
  # user; rootful docker: as root). Empty runtime -> the pg service no-ops.
  ensure_container_runtime
  local pg_runtime_bin="" pg_container="lunarwing-pg-$name"
  [[ -n "${CONTAINER_RT:-}" ]] && pg_runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  local pg_uid pg_home
  pg_uid="$(id -u "$name" 2>/dev/null || echo "")"
  pg_home="$(tenant_home "$name")"

  # ── Postgres container init script (dedicated service; the daemon needs it) ──
  # A first-class unit (not a daemon start_pre side-effect) so the host self-heal
  # pipeline — which auto-discovers /etc/init.d units — can remediate a crashed
  # Postgres independently.
  cat >"$OPENRC_INITD_DIR/lunarwing-pg-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing Postgres container ($name)"

: "\${pg_runtime:=$pg_runtime_bin}"
: "\${pg_container:=$pg_container}"
: "\${pg_rootless:=$MT_ROOTLESS}"
: "\${pg_user:=$name}"
: "\${pg_home:=$pg_home}"
: "\${pg_uid:=$pg_uid}"
: "\${pg_wait:=60}"

depend() {
    need net localmount
    after firewall
    before lunarwing-${name}
}

# Run the container runtime for this tenant's container: rootless -> as the
# tenant user with their runtime dir + HOME; rootful -> as root unchanged.
_pg() {
    if [ "\${pg_rootless}" = "true" ]; then
        sudo -u "\${pg_user}" env HOME="\${pg_home}" XDG_RUNTIME_DIR="/run/user/\${pg_uid}" "\${pg_runtime}" "\$@"
    else
        "\${pg_runtime}" "\$@"
    fi
}

start() {
    [ -n "\${pg_runtime}" ] && [ -x "\${pg_runtime}" ] || { ewarn "no container runtime; skipping Postgres for $name"; return 0; }
    ebegin "Starting Postgres container (\${pg_container})"
    if [ "\${pg_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${pg_user}:\${pg_user}" "/run/user/\${pg_uid}"
    fi
    _pg start "\${pg_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _pg exec "\${pg_container}" pg_isready -U lunarwing -q 2>/dev/null; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${pg_wait}" ] || { eend 1 "Postgres not ready after \${pg_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${pg_runtime}" ] && [ -x "\${pg_runtime}" ] || return 0
    ebegin "Stopping Postgres container (\${pg_container})"
    _pg stop --time 30 "\${pg_container}" >/dev/null 2>&1
    eend 0
}

status() {
    # Emit the standard OpenRC "started"/"stopped" wording (not "running") so the
    # health-check parser (grep started|stopped) and the mt-admin status display
    # classify the container correctly instead of relying on the rc_exit fallback.
    # "started" requires the container be running AND Postgres actually accept
    # connections (pg_isready) — a Running-but-wedged DB (crash recovery, disk
    # full, max_connections) otherwise reports healthy and is never remediated.
    # Mirrors the worker units' _wk_healthy and this unit's own start() gate.
    if [ "\$(_pg inspect -f '{{.State.Running}}' "\${pg_container}" 2>/dev/null)" = "true" ] \\
       && _pg exec "\${pg_container}" pg_isready -U lunarwing -q -t 3 2>/dev/null; then
        einfo "\${pg_container}: started"; return 0
    fi
    einfo "\${pg_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-pg-${name}"
  render_container_babysitter_unit "$name" pg "$pg_container" "$pg_uid" "$pg_home"

  # ── Main daemon init script ──
  # Work item #54: convert the workload CPU quota for rc_cgroup_settings.
  # Empty when WORKLOAD_CPU_QUOTA is not a plain "<n>%" — the cpu.max knob is
  # then skipped while memory/pids ceilings still apply.
  local openrc_cpu_max
  openrc_cpu_max="$(_openrc_cpu_max_from_quota "$WORKLOAD_CPU_QUOTA" || true)"

  cat >"$OPENRC_INITD_DIR/lunarwing-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing AI assistant ($name)"

: "\${lunarwing_command:=$repo/target/${PROFILE}/lunarwing}"
: "\${lunarwing_args:=--no-onboard run}"
: "\${lunarwing_user:=$name}"
: "\${lunarwing_group:=$name}"
: "\${lunarwing_workdir:=$repo}"
: "\${lunarwing_pidfile:=$run_dir/lunarwing.pid}"
: "\${lunarwing_state_dir:=$state_dir}"
: "\${lunarwing_runtime_dir:=$run_dir}"
: "\${lunarwing_log_dir:=$log_dir}"
: "\${lunarwing_output_log:=\${lunarwing_log_dir}/lunarwing.log}"
: "\${lunarwing_error_log:=\${lunarwing_log_dir}/lunarwing.err}"
: "\${lunarwing_env_file:=$env_dir/lunarwing.env}"
: "\${lunarwing_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${lunarwing_umask:=0077}"
: "\${lunarwing_respawn_delay:=5}"
: "\${lunarwing_respawn_max:=5}"
: "\${lunarwing_respawn_period:=60}"
: "\${lunarwing_retry:=SIGTERM/30/KILL/5}"

# Work item #54: hard cgroup v2 ceilings — the OpenRC equivalent of the
# systemd daemon-unit caps plus lunarwing-workloads.slice (BUG-005). OpenRC
# has no transient scopes, so the daemon and its agent-spawned workloads share
# this one service cgroup; the defaults are therefore the workload outer
# bounds, not the smaller daemon-only caps. Legacy/cgroup-less hosts retain the
# portable process-tree fallback. On cgroup v2, start_pre verifies every
# configured effective value and fails startup if OpenRC skipped a controller.
# The stop wrapper supplies cleanup parity even on OpenRC releases affected by
# upstream issue #910 and still runs when supervise-daemon stop reports failure.
: "\${lunarwing_cgroup_limits_enabled:=true}"
: "\${lunarwing_cgroup_memory_high:=$WORKLOAD_MEMORY_HIGH}"
: "\${lunarwing_cgroup_memory_max:=$WORKLOAD_MEMORY_MAX}"
: "\${lunarwing_cgroup_cpu_max:=$openrc_cpu_max}"
: "\${lunarwing_cgroup_pids_max:=$WORKLOAD_TASKS_MAX}"

_lunarwing_cgroup_memory_bytes() {
    _lw_raw="\$1"
    case "\${_lw_raw}" in
        *[Kk]) _lw_number="\${_lw_raw%?}"; _lw_multiplier=1024; _lw_max=9007199254740991 ;;
        *[Mm]) _lw_number="\${_lw_raw%?}"; _lw_multiplier=1048576; _lw_max=8796093022207 ;;
        *[Gg]) _lw_number="\${_lw_raw%?}"; _lw_multiplier=1073741824; _lw_max=8589934591 ;;
        *[Tt]) _lw_number="\${_lw_raw%?}"; _lw_multiplier=1099511627776; _lw_max=8388607 ;;
        *)     _lw_number="\${_lw_raw}"; _lw_multiplier=1; _lw_max=999999999999999999 ;;
    esac
    case "\${_lw_number}" in
        ""|*[!0-9]*) return 1 ;;
    esac
    while [ "\${_lw_number#0}" != "\${_lw_number}" ] && [ "\${#_lw_number}" -gt 1 ]; do
        _lw_number="\${_lw_number#0}"
    done
    [ "\${_lw_number}" -gt 0 ] 2>/dev/null || return 1
    [ "\${#_lw_number}" -le "\${#_lw_max}" ] || return 1
    [ "\${_lw_number}" -le "\${_lw_max}" ] 2>/dev/null || return 1
    printf '%s\n' "\$((_lw_number * _lw_multiplier))"
}

rc_cgroup_cleanup="YES"
rc_cgroup_settings=""
_lw_cgroup_config_error=""
if yesno "\${lunarwing_cgroup_limits_enabled}"; then
    if ! _lw_memory_high="\$(_lunarwing_cgroup_memory_bytes "\${lunarwing_cgroup_memory_high}")"; then
        _lw_cgroup_config_error="invalid lunarwing_cgroup_memory_high: \${lunarwing_cgroup_memory_high}"
    elif ! _lw_memory_max="\$(_lunarwing_cgroup_memory_bytes "\${lunarwing_cgroup_memory_max}")"; then
        _lw_cgroup_config_error="invalid lunarwing_cgroup_memory_max: \${lunarwing_cgroup_memory_max}"
    elif [ "\${_lw_memory_high}" -gt "\${_lw_memory_max}" ]; then
        _lw_cgroup_config_error="lunarwing_cgroup_memory_high exceeds lunarwing_cgroup_memory_max"
    else
        rc_cgroup_settings="
memory.high \${_lw_memory_high}
memory.max \${_lw_memory_max}
cpu.max \${lunarwing_cgroup_cpu_max}
pids.max \${lunarwing_cgroup_pids_max}
"
    fi
fi

_lunarwing_service_cgroup_path() {
    _lw_root="\${1:-/sys/fs/cgroup}"
    for _lw_candidate in \
        "\${_lw_root}/openrc.\${RC_SVCNAME}" \
        "\${_lw_root}/unified/openrc.\${RC_SVCNAME}"
    do
        if [ -d "\${_lw_candidate}" ]; then
            printf '%s\n' "\${_lw_candidate}"
            return 0
        fi
    done
    return 1
}

_lunarwing_cgroup_value_matches() {
    _lw_cg="\$1"
    _lw_knob="\$2"
    _lw_expected="\$3"
    [ -n "\${_lw_expected}" ] || {
        eerror "LunarWing service cgroup value is empty: \${_lw_knob}"
        return 1
    }
    [ -r "\${_lw_cg}/\${_lw_knob}" ] || {
        eerror "LunarWing service cgroup missing \${_lw_knob}: \${_lw_cg}"
        return 1
    }
    _lw_actual="\$(cat "\${_lw_cg}/\${_lw_knob}")"
    [ "\${_lw_actual}" = "\${_lw_expected}" ] || {
        eerror "LunarWing service cgroup \${_lw_knob} mismatch: expected \${_lw_expected}, got \${_lw_actual}"
        return 1
    }
}

_lunarwing_validate_service_cgroup() {
    _lw_root="\${1:-/sys/fs/cgroup}"
    # Preserve legacy/cgroup-less startup. On a cgroup v2 host, enabled limits
    # are fail-closed so OpenRC cannot silently skip a missing controller.
    if [ ! -r "\${_lw_root}/cgroup.controllers" ] \
        && [ ! -r "\${_lw_root}/unified/cgroup.controllers" ]; then
        return 0
    fi
    _lw_cg="\$(_lunarwing_service_cgroup_path "\${_lw_root}")" || {
        eerror "OpenRC did not create LunarWing service cgroup for \${RC_SVCNAME}"
        return 1
    }
    _lunarwing_cgroup_value_matches "\${_lw_cg}" memory.high "\${_lw_memory_high}" || return 1
    _lunarwing_cgroup_value_matches "\${_lw_cg}" memory.max "\${_lw_memory_max}" || return 1
    if [ -n "\${lunarwing_cgroup_cpu_max}" ]; then
        _lunarwing_cgroup_value_matches "\${_lw_cg}" cpu.max "\${lunarwing_cgroup_cpu_max}" || return 1
    fi
    _lunarwing_cgroup_value_matches "\${_lw_cg}" pids.max "\${lunarwing_cgroup_pids_max}"
}

_lunarwing_cgroup_has_other_members() {
    _lw_cg="\$1"
    while IFS= read -r _lw_pid; do
        [ -n "\${_lw_pid}" ] || continue
        [ "\${_lw_pid}" = "\$\$" ] || return 0
    done <"\${_lw_cg}/cgroup.procs"
    return 1
}

_lunarwing_service_cgroup_cleanup() {
    _LUNARWING_CGROUP_CLEANUP_HANDLED=false
    _lw_root="\${1:-/sys/fs/cgroup}"
    _lw_cg="\$(_lunarwing_service_cgroup_path "\${_lw_root}")" || return 0
    _lw_parent="\${_lw_cg%/openrc.*}"
    [ -r "\${_lw_cg}/cgroup.procs" ] || {
        ewarn "cannot inspect LunarWing service cgroup: \${_lw_cg}"
        return 1
    }

    # Move this openrc-run process out before using cgroup.kill; otherwise the
    # cleanup syscall would kill its own stop transaction.
    if [ -w "\${_lw_parent}/cgroup.procs" ] && [ -w "\${_lw_cg}/cgroup.kill" ] \
        && printf '%s' "\$\$" >"\${_lw_parent}/cgroup.procs" 2>/dev/null \
        && printf '1' >"\${_lw_cg}/cgroup.kill" 2>/dev/null \
        && ! _lunarwing_cgroup_has_other_members "\${_lw_cg}"; then
        _LUNARWING_CGROUP_CLEANUP_HANDLED=true
        return 0
    fi

    _lw_tries=0
    while _lunarwing_cgroup_has_other_members "\${_lw_cg}"; do
        if [ "\${_lw_tries}" -ge 5 ]; then
            ewarn "leftover processes survived LunarWing cgroup cleanup: \${_lw_cg}"
            return 1
        fi
        while IFS= read -r _lw_pid; do
            [ -n "\${_lw_pid}" ] || continue
            [ "\${_lw_pid}" = "\$\$" ] && continue
            kill -KILL "\${_lw_pid}" 2>/dev/null || true
        done <"\${_lw_cg}/cgroup.procs"
        _lw_tries=\$((_lw_tries + 1))
        sleep 1
    done
    _LUNARWING_CGROUP_CLEANUP_HANDLED=true
}

stop() {
    _lw_stop_rc=0
    _lw_cleanup_rc=0
    default_stop || _lw_stop_rc=\$?
    _lunarwing_service_cgroup_cleanup || _lw_cleanup_rc=\$?
    # Our compatibility path handled v2. Leave native cleanup enabled for v1
    # or hosts where no v2 service cgroup was present.
    if [ "\${_LUNARWING_CGROUP_CLEANUP_HANDLED}" = true ]; then
        rc_cgroup_cleanup="NO"
    fi
    [ "\${_lw_cleanup_rc}" -eq 0 ] || return "\${_lw_cleanup_rc}"
    return "\${_lw_stop_rc}"
}

command="\${lunarwing_openrc_env_exec}"
command_args="--env-file \${lunarwing_env_file} -- ${relay_exec_prefix}\${lunarwing_command} \${lunarwing_args}"
command_user="\${lunarwing_user}:\${lunarwing_group}"
directory="\${lunarwing_workdir}"
pidfile="\${lunarwing_pidfile}"
supervisor="supervise-daemon"
retry="\${lunarwing_retry}"
respawn_delay="\${lunarwing_respawn_delay}"
respawn_max="\${lunarwing_respawn_max}"
respawn_period="\${lunarwing_respawn_period}"
output_log="\${lunarwing_output_log}"
error_log="\${lunarwing_error_log}"
required_files="\${lunarwing_openrc_env_exec} \${lunarwing_command} \${lunarwing_env_file}"

depend() {
    need net localmount lunarwing-pg-${name}
    want \${lunarwing_rc_need}
    use dns logger
    after firewall lunarwing-pg-${name} xmpp-bridge-${name}${proxy_rc_after}${weechat_rc_after}${darkirc_rc_after}
}

start_pre() {
    if [ -n "\${_lw_cgroup_config_error}" ]; then
        eerror "\${_lw_cgroup_config_error}"
        return 1
    fi
    if yesno "\${lunarwing_cgroup_limits_enabled}"; then
        _lunarwing_validate_service_cgroup || return 1
    fi
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_state_dir}"
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_log_dir}"
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_runtime_dir}"
    checkpath -f -m 0640 -o "\${lunarwing_user}:\${lunarwing_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${lunarwing_user}:\${lunarwing_group}" "\${error_log}"
    # Postgres is brought up by the dedicated lunarwing-pg-${name} service, which
    # this unit declares as a hard dependency (need), so the DB is already up.
    umask "\${lunarwing_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-${name}"

  # ── XMPP bridge init script ──
  cat >"$OPENRC_INITD_DIR/xmpp-bridge-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing XMPP bridge ($name)"

: "\${xmpp_bridge_command:=$repo/bridges/xmpp-bridge/target/${PROFILE}/xmpp-bridge}"
: "\${xmpp_bridge_user:=$name}"
: "\${xmpp_bridge_group:=$name}"
: "\${xmpp_bridge_workdir:=$repo/bridges/xmpp-bridge}"
: "\${xmpp_bridge_pidfile:=$run_dir/xmpp-bridge.pid}"
: "\${xmpp_bridge_state_dir:=$state_dir}"
: "\${xmpp_bridge_runtime_dir:=$run_dir}"
: "\${xmpp_bridge_log_dir:=$log_dir}"
: "\${xmpp_bridge_output_log:=\${xmpp_bridge_log_dir}/xmpp-bridge.log}"
: "\${xmpp_bridge_error_log:=\${xmpp_bridge_log_dir}/xmpp-bridge.err}"
: "\${xmpp_bridge_env_file:=$env_dir/xmpp-bridge.env}"
: "\${xmpp_bridge_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${xmpp_bridge_umask:=0077}"
: "\${xmpp_bridge_respawn_delay:=5}"
: "\${xmpp_bridge_respawn_max:=5}"
: "\${xmpp_bridge_respawn_period:=60}"
: "\${xmpp_bridge_retry:=SIGTERM/30/KILL/5}"

command="\${xmpp_bridge_openrc_env_exec}"
command_args="--env-file \${xmpp_bridge_env_file} -- \${xmpp_bridge_command}"
command_user="\${xmpp_bridge_user}:\${xmpp_bridge_group}"
directory="\${xmpp_bridge_workdir}"
pidfile="\${xmpp_bridge_pidfile}"
supervisor="supervise-daemon"
retry="\${xmpp_bridge_retry}"
respawn_delay="\${xmpp_bridge_respawn_delay}"
respawn_max="\${xmpp_bridge_respawn_max}"
respawn_period="\${xmpp_bridge_respawn_period}"
output_log="\${xmpp_bridge_output_log}"
error_log="\${xmpp_bridge_error_log}"
required_files="\${xmpp_bridge_openrc_env_exec} \${xmpp_bridge_command} \${xmpp_bridge_env_file}"

depend() {
    need net localmount
    use dns logger
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_state_dir}"
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_log_dir}"
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_runtime_dir}"
    checkpath -f -m 0640 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${error_log}"
    umask "\${xmpp_bridge_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/xmpp-bridge-${name}"

  # ── DarkIRC daemon init script (only when enabled) ──
  if tenant_darkirc_enabled "$name"; then
  cat >"$OPENRC_INITD_DIR/lunarwing-darkirc-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing DarkIRC daemon ($name)"

darkirc_command="$DARKIRC_RELEASE_DAEMON"
: "\${darkirc_args:=--config $state_dir/darkirc/darkirc_config.toml}"
: "\${darkirc_user:=$name}"
: "\${darkirc_group:=$name}"
: "\${darkirc_workdir:=$state_dir/darkirc}"
: "\${darkirc_pidfile:=$run_dir/darkirc.pid}"
: "\${darkirc_state_dir:=$state_dir}"
: "\${darkirc_runtime_dir:=$run_dir}"
: "\${darkirc_log_dir:=$log_dir}"
: "\${darkirc_output_log:=\${darkirc_log_dir}/darkirc.log}"
: "\${darkirc_error_log:=\${darkirc_log_dir}/darkirc.err}"
: "\${darkirc_umask:=0077}"
: "\${darkirc_respawn_delay:=5}"
: "\${darkirc_respawn_max:=5}"
: "\${darkirc_respawn_period:=60}"
: "\${darkirc_retry:=SIGTERM/30/KILL/5}"

command="\${darkirc_command}"
command_args="\${darkirc_args}"
command_user="\${darkirc_user}:\${darkirc_group}"
directory="\${darkirc_workdir}"
pidfile="\${darkirc_pidfile}"
supervisor="supervise-daemon"
retry="\${darkirc_retry}"
respawn_delay="\${darkirc_respawn_delay}"
respawn_max="\${darkirc_respawn_max}"
respawn_period="\${darkirc_respawn_period}"
output_log="\${darkirc_output_log}"
error_log="\${darkirc_error_log}"
required_files="\${command}"

depend() {
    need net localmount
    use dns logger
    after firewall
    before lunarwing-darkirc-adapter-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_state_dir}/darkirc"
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_log_dir}"
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_runtime_dir}"
    checkpath -f -m 0640 -o "\${darkirc_user}:\${darkirc_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${darkirc_user}:\${darkirc_group}" "\${error_log}"
    umask "\${darkirc_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-darkirc-${name}"
  fi

  # ── TensorZero proxy init script (only when enabled) ──
  # Validate the tenant's upstream as data, but NEVER bake it into this root-
  # parsed shell script (WI#376). env-exec loads proxy.env after command_user
  # drops privileges; the proxy reads TENSORZERO_URL without shell evaluation.
  if tenant_proxy_enabled "$name"; then
  validate_openrc_proxy_env "$name" || return 1
  render_proxy_openrc_unit "$name" || return 1
  fi

  if [[ "$weechat_enabled" == "true" ]]; then
    # ── WeeChat init script (tmux-based) ──
    _render_weechat_openrc_unit "$name" "$OPENRC_INITD_DIR/lunarwing-weechat-${name}"

    # WeeChat WS adapter init script
    cat >"$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing WeeChat WS adapter ($name)"

: "\${adapter_command:=$(command -v python3)}"
: "\${adapter_args:=$ws_adapter_path}"
: "\${adapter_user:=$name}"
: "\${adapter_group:=$name}"
: "\${adapter_pidfile:=$run_dir/weechat-adapter.pid}"
: "\${adapter_runtime_dir:=$run_dir}"
: "\${adapter_log_dir:=$log_dir}"
: "\${adapter_output_log:=\${adapter_log_dir}/weechat-adapter.log}"
: "\${adapter_error_log:=\${adapter_log_dir}/weechat-adapter.err}"
: "\${adapter_env_file:=$env_dir/lunarwing.env}"
: "\${adapter_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${adapter_umask:=0077}"
: "\${adapter_respawn_delay:=5}"
: "\${adapter_respawn_max:=5}"
: "\${adapter_respawn_period:=60}"
: "\${adapter_retry:=SIGTERM/30/KILL/5}"

command="\${adapter_openrc_env_exec}"
command_args="--env-file \${adapter_env_file} -- ${relay_exec_prefix}\${adapter_command} \${adapter_args}"
command_user="\${adapter_user}:\${adapter_group}"
directory="$ws_adapter_dir"
pidfile="\${adapter_pidfile}"
supervisor="supervise-daemon"
retry="\${adapter_retry}"
respawn_delay="\${adapter_respawn_delay}"
respawn_max="\${adapter_respawn_max}"
respawn_period="\${adapter_respawn_period}"
output_log="\${adapter_output_log}"
error_log="\${adapter_error_log}"
required_files="\${adapter_openrc_env_exec} \${adapter_command} \${adapter_env_file}"

depend() {
    need net lunarwing-weechat-${name}
    use dns
    after firewall lunarwing-weechat-${name}
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${adapter_user}:\${adapter_group}" "\${adapter_runtime_dir}"
    checkpath -d -m 0750 -o "\${adapter_user}:\${adapter_group}" "\${adapter_log_dir}"
    checkpath -f -m 0640 -o "\${adapter_user}:\${adapter_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${adapter_user}:\${adapter_group}" "\${error_log}"
    umask "\${adapter_umask}"
}
INITEOF
    chmod 0755 "$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}"
  else
    rm -f \
      "$OPENRC_INITD_DIR/lunarwing-weechat-${name}" \
      "$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}" \
      "$OPENRC_CONFD_DIR/lunarwing-weechat-${name}" \
      "$OPENRC_CONFD_DIR/lunarwing-weechat-adapter-${name}" \
      "$OPENRC_RUNLEVELS_DIR"/*/"lunarwing-weechat-${name}" \
      "$OPENRC_RUNLEVELS_DIR"/*/"lunarwing-weechat-adapter-${name}"
  fi

  # DarkIRC adapter init script (only when enabled)
  if tenant_darkirc_enabled "$name"; then
  local darkirc_adapter_path darkirc_adapter_dir
  darkirc_adapter_path="$(tenant_lw_root "$name")/darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py"
  darkirc_adapter_dir="$(dirname "$darkirc_adapter_path")"

  cat >"$OPENRC_INITD_DIR/lunarwing-darkirc-adapter-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing DarkIRC adapter ($name)"

: "\${darkirc_adapter_command:=$(command -v python3)}"
: "\${darkirc_adapter_args:=$darkirc_adapter_path}"
: "\${darkirc_adapter_user:=$name}"
: "\${darkirc_adapter_group:=$name}"
: "\${darkirc_adapter_pidfile:=$run_dir/darkirc-adapter.pid}"
: "\${darkirc_adapter_runtime_dir:=$run_dir}"
: "\${darkirc_adapter_log_dir:=$log_dir}"
: "\${darkirc_adapter_output_log:=\${darkirc_adapter_log_dir}/darkirc-adapter.log}"
: "\${darkirc_adapter_error_log:=\${darkirc_adapter_log_dir}/darkirc-adapter.err}"
: "\${darkirc_adapter_env_file:=$env_dir/darkirc-adapter.env}"
: "\${darkirc_adapter_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${darkirc_adapter_umask:=0077}"
: "\${darkirc_adapter_respawn_delay:=5}"
: "\${darkirc_adapter_respawn_max:=5}"
: "\${darkirc_adapter_respawn_period:=60}"
: "\${darkirc_adapter_retry:=SIGTERM/30/KILL/5}"

command="\${darkirc_adapter_openrc_env_exec}"
command_args="--env-file \${darkirc_adapter_env_file} -- \${darkirc_adapter_command} \${darkirc_adapter_args}"
command_user="\${darkirc_adapter_user}:\${darkirc_adapter_group}"
directory="$darkirc_adapter_dir"
pidfile="\${darkirc_adapter_pidfile}"
supervisor="supervise-daemon"
retry="\${darkirc_adapter_retry}"
respawn_delay="\${darkirc_adapter_respawn_delay}"
respawn_max="\${darkirc_adapter_respawn_max}"
respawn_period="\${darkirc_adapter_respawn_period}"
output_log="\${darkirc_adapter_output_log}"
error_log="\${darkirc_adapter_error_log}"
required_files="\${darkirc_adapter_openrc_env_exec} \${darkirc_adapter_command} \${darkirc_adapter_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${darkirc_adapter_runtime_dir}"
    checkpath -d -m 0750 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${darkirc_adapter_log_dir}"
    checkpath -f -m 0640 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${error_log}"
    umask "\${darkirc_adapter_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-darkirc-adapter-${name}"
  fi

  # ── Conf.d files ──
  local darkirc_rc_need=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_rc_need=" lunarwing-darkirc-adapter-${name}"
  fi
  local proxy_rc_need=""
  if tenant_proxy_enabled "$name"; then
    proxy_rc_need=" lunarwing-proxy-${name}"
  fi
  local weechat_rc_need=""
  if [[ "$weechat_enabled" == "true" ]]; then
    weechat_rc_need=" lunarwing-weechat-${name} lunarwing-weechat-adapter-${name}"
  fi
  _render_openrc_lunarwing_confd "$name" \
    "xmpp-bridge-${name}${proxy_rc_need}${weechat_rc_need}${darkirc_rc_need}"

  cat >"$OPENRC_CONFD_DIR/xmpp-bridge-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
xmpp_bridge_rc_before="lunarwing-${name}"
CONFD

  if tenant_proxy_enabled "$name"; then
    cat >"$OPENRC_CONFD_DIR/lunarwing-proxy-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  if [[ "$weechat_enabled" == "true" ]]; then
    cat >"$OPENRC_CONFD_DIR/lunarwing-weechat-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD

    cat >"$OPENRC_CONFD_DIR/lunarwing-weechat-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  if tenant_darkirc_enabled "$name"; then
    cat >"$OPENRC_CONFD_DIR/lunarwing-darkirc-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  render_vision_openrc_assets "$name" || return 1
  say "rendered OpenRC init scripts and conf.d for $name"
}

start_tenant_openrc() {
  local name="$1"
  require_safe_openrc_proxy_units || return 1
  # Record operator intent UP FRONT: enabling the primary daemon in the default
  # runlevel marks this tenant as "started" (boot-persistent) regardless of whether
  # any unit's FIRST start succeeds. health-openrc.sh's started-gate
  # (tenant_started) keys on exactly this, so a daemon that crashes on its first
  # start is still reported `critical` (a real outage) and remediated — not masked
  # as `skipped`. The selective rc-update-add loop below additionally boot-enables
  # the optional units that actually came up. Idempotent.
  _openrc_update add "lunarwing-${name}" default >/dev/null 2>&1 || true

  # Postgres first: the daemon `need`s it (and it's idempotent if already up).
  _openrc_service "lunarwing-pg-${name}" start
  # Start PG babysitter (supervises the container via podman wait)
  _openrc_service "lunarwing-pg-${name}-sup" start 2>/dev/null || true
  # Optional channels next, non-fatal: a missing weechat/aiohttp must not abort
  # the core stack (the main daemon does not depend on them). WeeChat units are
  # skipped entirely when the tenant's registry flag disables them (ISSUE-006):
  # start-tenant must not reactivate a service the operator turned off.
  if tenant_weechat_enabled "$name"; then
    _openrc_service "lunarwing-weechat-${name}" start 2>/dev/null || say "  (lunarwing-weechat-${name} skipped — optional)"
    _openrc_service "lunarwing-weechat-adapter-${name}" start 2>/dev/null || say "  (lunarwing-weechat-adapter-${name} skipped — optional)"
  else
    say "  (weechat disabled for $name — not starting lunarwing-weechat{,-adapter}-${name})"
  fi
  if tenant_darkirc_enabled "$name"; then
    _openrc_service "lunarwing-darkirc-${name}" start \
      || die "failed to start OpenRC service lunarwing-darkirc-${name}"
    _openrc_service "lunarwing-darkirc-adapter-${name}" start \
      || die "failed to start OpenRC service lunarwing-darkirc-adapter-${name}"
  fi
  if tenant_proxy_enabled "$name"; then
    _openrc_service "lunarwing-proxy-${name}" start
  fi
  _openrc_service "xmpp-bridge-${name}" start
  _openrc_service "lunarwing-${name}" start
  # Remove stale worker supervision for unselected or intentionally stopped
  # workers. Selected running workers register/start their own units after the
  # daemon is ready, preserving the SSH-agent socket ordering.
  for worker in pebble opencode kimi opencode2; do
    if tenant_worker_openrc_stop_requested "$name" "$worker"; then
      openrc_worker_stop_disable "$name" "$worker" \
        || die "failed to disable OpenRC worker $worker for tenant '$name'"
    fi
  done
  say "OpenRC services started for $name"

  # Auto-enable on boot whatever is actually running (idempotent, OpenRC only).
  local svc
  local boot_svcs=("lunarwing-pg-${name}" "lunarwing-pg-${name}-sup" "xmpp-bridge-${name}" "lunarwing-${name}" "lunarwing-voice-${name}" "lunarwing-voice-${name}-sup")
  if ! tenant_vision_external "$name"; then
    boot_svcs+=("lunarwing-vision-${name}" "lunarwing-vision-${name}-sup")
  fi
  if tenant_weechat_enabled "$name"; then
    boot_svcs+=("lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
  fi
  if tenant_proxy_enabled "$name"; then
    boot_svcs+=("lunarwing-proxy-${name}")
  fi
  if tenant_darkirc_enabled "$name"; then
    boot_svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
  fi
  for svc in "${boot_svcs[@]}"; do
    if _openrc_service "$svc" status >/dev/null 2>&1; then
      _openrc_update add "$svc" default >/dev/null 2>&1 || true
    fi
  done
  say "enabled boot persistence (default runlevel) for $name's running services"
}

_openrc_stop_service() {
  local service="$1"
  if _openrc_service "$service" stop; then
    return 0
  fi
  say "failed to stop OpenRC service $service"
  return 1
}

_openrc_stop_service_if_installed() {
  local service="$1"
  local path="$OPENRC_INITD_DIR/$service"
  # Only a truly absent unit is "already stopped" (EQ-2). A present but
  # non-regular or symlinked entry is not ours to classify (FL-3): refuse
  # rather than let remove-tenant/upgrade-tenant proceed without a stop.
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    return 0
  fi
  if [[ ! -f "$path" || -L "$path" ]]; then
    say "refusing to skip stop: $path exists but is not a regular init script"
    return 1
  fi
  _openrc_stop_service "$service"
}

stop_tenant_openrc() {
  local name="$1"
  require_safe_openrc_proxy_units || return 1
  local stop_failed=false worker
  # Every unit is stopped only when its init script is installed: an absent
  # unit is already stopped, so a partially provisioned or partially removed
  # tenant never makes remove-tenant/upgrade-tenant permanently refuse (EQ-2).
  # A refusal (125) or a failed stop of an installed unit still fails the stop.
  _openrc_stop_service_if_installed "lunarwing-${name}" || stop_failed=true
  _openrc_stop_service_if_installed "xmpp-bridge-${name}" || stop_failed=true
  _openrc_stop_service_if_installed "lunarwing-proxy-${name}" || stop_failed=true
  _openrc_stop_service_if_installed "lunarwing-weechat-adapter-${name}" || stop_failed=true
  _openrc_stop_service_if_installed "lunarwing-weechat-${name}" || stop_failed=true
  if tenant_darkirc_enabled "$name"; then
    _openrc_stop_service_if_installed "lunarwing-darkirc-adapter-${name}" || stop_failed=true
    _openrc_stop_service_if_installed "lunarwing-darkirc-${name}" || stop_failed=true
  fi
  # Container babysitters must stop before their containers or they immediately respawn them.
  for worker in nanocode pebble opencode kimi opencode2 vision voice; do
    _openrc_stop_service_if_installed "lunarwing-${worker}-${name}-sup" || stop_failed=true
  done
  _openrc_stop_service_if_installed "lunarwing-vision-${name}" || stop_failed=true
  _openrc_stop_service_if_installed "lunarwing-voice-${name}" || stop_failed=true
  # Postgres last: the daemon depends on it, so it stops after its consumers.
  _openrc_stop_service_if_installed "lunarwing-pg-${name}-sup" || stop_failed=true
  _openrc_stop_service_if_installed "lunarwing-pg-${name}" || stop_failed=true
  if [[ "$stop_failed" == true ]]; then
    say "OpenRC tenant shutdown completed with service stop errors for $name"
    return 1
  fi
  say "OpenRC services stopped for $name"
}

cleanup_legacy_weechat_openrc() {
  local name="$1" service init_path conf_path link runlevel
  local changed=false
  service="weechat-${name}"
  init_path="$OPENRC_INITD_DIR/$service"
  conf_path="$OPENRC_CONFD_DIR/$service"

  if [[ -e "$init_path" || -L "$init_path" ]]; then
    [[ -f "$init_path" && ! -L "$init_path" ]] \
      || die "refusing to operate on non-regular legacy OpenRC service: $init_path"
    _openrc_service "$service" stop || return 1
  fi

  for link in "$OPENRC_RUNLEVELS_DIR"/*/"$service"; do
    [[ -e "$link" || -L "$link" ]] || continue
    [[ -L "$link" ]] || die "refusing to remove non-symlink legacy runlevel entry: $link"
    runlevel="$(basename "$(dirname "$link")")"
    _openrc_update del "$service" "$runlevel" || return 1
    if [[ -e "$link" || -L "$link" ]]; then
      rm -- "$link" || return 1
    fi
    changed=true
  done

  for link in "$init_path" "$conf_path"; do
    [[ -e "$link" || -L "$link" ]] || continue
    [[ -f "$link" && ! -L "$link" ]] \
      || die "refusing to remove non-regular legacy OpenRC path: $link"
    rm -- "$link" || return 1
    changed=true
  done

  if [[ "$changed" == true ]]; then
    say "removed legacy OpenRC service $service for $name"
  else
    say "legacy OpenRC service $service absent for $name"
  fi
}

uninstall_tenant_openrc() {
  local name="$1" svc failed=false
  for svc in "lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}" "lunarwing-darkirc-adapter-${name}" "lunarwing-darkirc-${name}" "lunarwing-pg-${name}" "lunarwing-nanocode-${name}" "lunarwing-pebble-${name}" "lunarwing-opencode-${name}" "lunarwing-opencode2-${name}" "lunarwing-kimi-${name}" "lunarwing-vision-${name}" "lunarwing-voice-${name}" "lunarwing-pg-${name}-sup" "lunarwing-nanocode-${name}-sup" "lunarwing-pebble-${name}-sup" "lunarwing-opencode-${name}-sup" "lunarwing-opencode2-${name}-sup" "lunarwing-kimi-${name}-sup" "lunarwing-vision-${name}-sup" "lunarwing-voice-${name}-sup"; do
    _openrc_update del "$svc" default 2>/dev/null || true
    # rm failures must reach the caller's '|| die': previously the trailing
    # say always returned 0, making that guard dead code (EQ-8).
    rm -f "$OPENRC_INITD_DIR/$svc" "$OPENRC_CONFD_DIR/$svc" || failed=true
  done
  if [[ "$failed" == true ]]; then
    say "failed to remove one or more OpenRC unit files for $name" >&2
    return 1
  fi
  say "uninstalled OpenRC services for $name"
}

openrc_service_remove_runlevels() {
  local svc="$1" link runlevel
  for link in "$OPENRC_RUNLEVELS_DIR"/*/"$svc"; do
    [[ -e "$link" || -L "$link" ]] || continue
    runlevel="$(basename "$(dirname "$link")")"
    _openrc_update del "$svc" "$runlevel" >/dev/null 2>&1 \
      || { say "failed to remove $svc from OpenRC runlevel $runlevel" >&2; return 1; }
  done
}
