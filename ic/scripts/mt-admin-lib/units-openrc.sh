#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# units-openrc.sh — part of the lunarwing-mt-admin library.
#
# OpenRC init-script + conf.d rendering and tenant service lifecycle,
# including the worker/babysitter/vision unit renderers and the
# lunarwing-openrc-env-exec installer. Mirror image of units-systemd.sh.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.

render_worker_openrc_unit() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  ensure_container_runtime
  local runtime_bin="" container="lunarwing-${worker}-${name}" uid home
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"

  cat >"/etc/init.d/${container}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing ${worker} worker ($name)"

: "\${wk_runtime:=$runtime_bin}"
: "\${wk_container:=$container}"
: "\${wk_rootless:=$MT_ROOTLESS}"
: "\${wk_user:=$name}"
: "\${wk_home:=$home}"
: "\${wk_uid:=$uid}"
: "\${wk_health_port:=$health_port}"
: "\${wk_wait:=60}"

depend() {
    need net localmount
    after firewall lunarwing-${name}
}

# Run the container runtime as the owning user (rootless) or root (rootful).
_wk() {
    if [ "\${wk_rootless}" = "true" ]; then
        sudo -u "\${wk_user}" env HOME="\${wk_home}" XDG_RUNTIME_DIR="/run/user/\${wk_uid}" "\${wk_runtime}" "\$@"
    else
        "\${wk_runtime}" "\$@"
    fi
}

# Healthy = container running AND its internal /health endpoint answers.
_wk_healthy() {
    [ "\$(_wk inspect -f '{{.State.Running}}' "\${wk_container}" 2>/dev/null)" = "true" ] || return 1
    _wk exec "\${wk_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${wk_health_port}/health" 2>/dev/null
}

start() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || { ewarn "no container runtime; skipping ${worker} for $name"; return 0; }
    ebegin "Starting ${worker} worker (\${wk_container})"
    if [ "\${wk_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${wk_user}:\${wk_user}" "/run/user/\${wk_uid}"
    fi
    _wk start "\${wk_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _wk_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${wk_wait}" ] || { eend 1 "${worker} worker not healthy after \${wk_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || return 0
    ebegin "Stopping ${worker} worker (\${wk_container})"
    _wk stop --time 30 "\${wk_container}" >/dev/null 2>&1
    eend 0
}

status() {
    # Standard OpenRC started/stopped wording so health-openrc.sh classifies it.
    if _wk_healthy; then
        einfo "\${wk_container}: started"; return 0
    fi
    einfo "\${wk_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/${container}"
}

# Promote a (rootless) worker container to a dedicated OpenRC unit so it is
# health-monitored, self-healed, and boot-persistent. No-op unless OpenRC.
_register_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  render_worker_openrc_unit "$name" "$worker"
  rc-update add "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rc-service "lunarwing-${worker}-${name}" start >/dev/null 2>&1 || true
  say "registered OpenRC unit lunarwing-${worker}-${name} (health-monitored, boot-persistent)"
  local container="lunarwing-${worker}-${name}"
  local uid home
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"
  _register_babysitter "$name" "$worker" "$container" "$uid" "$home"
}

# Tear down a worker's OpenRC unit (boot-disable + remove the init script).
_deregister_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/lunarwing-${worker}-${name}" ]] || return 0
  _deregister_babysitter "lunarwing-${worker}-${name}"
  rc-service "lunarwing-${worker}-${name}" stop >/dev/null 2>&1 || true
  rc-update del "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rm -f "/etc/init.d/lunarwing-${worker}-${name}" "/etc/conf.d/lunarwing-${worker}-${name}"
}

# Install (or refresh) the babysitter helper binary from this repo to /usr/local/sbin.
# Idempotent: skips if the on-disk copy is byte-identical (mtime/perm-check), so
# `start_tenant` is safe to call repeatedly without churning the file. This
# decouples helper provisioning from the watchdog installer — a tenant created
# before the watchdog is installed still gets a working babysitter.
ensure_babysitter_helper() {
  local src="${SCRIPT_DIR}/lunarwing-ctr-babysit.sh"
  local dst="/usr/local/sbin/lunarwing-ctr-babysit"
  [[ -f "$src" ]] || {
    say "WARNING: babysitter helper source missing: $src (skipping install)"
    return 0
  }
  if [[ -f "$dst" ]] && cmp -s "$src" "$dst"; then
    return 0  # up to date; don't touch mtime/perm
  fi
  install -o root -g root -m 0755 "$src" "$dst" \
    && say "Installed babysitter helper: $dst" \
    || say "WARNING: failed to install babysitter helper to $dst"
}

# Render a supervised babysitter OpenRC unit for a rootless container.
# The babysitter blocks on `podman wait <container>` and respawns via supervise-daemon
# when the container exits, providing docker-parity crash recovery (~seconds, not minutes).
# Only applicable to rootless podman on OpenRC (systemd uses Quadlet).
# Usage: render_container_babysitter_unit <tenant> <container-type> <container-name> <uid> <home>
# Example: render_container_babysitter_unit acme pg lunarwing-pg-acme 1001 /home/acme
render_container_babysitter_unit() {
  local name="$1" type="$2" container="$3" uid="$4" home="$5"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  # The pg path renders directly (bypassing _register_babysitter), so install the
  # helper here too — otherwise a worker-less tenant renders a -sup unit whose
  # required_files=<helper> never exists and supervise-daemon silently never starts it.
  ensure_babysitter_helper
  local babysitter="/etc/init.d/${container}-sup"
  # Log into the tenant's existing logs dir (created at add-tenant, tenant-owned),
  # exactly like every other unit (lunarwing/proxy/xmpp-bridge). The old
  # /var/log/lunarwing/<t> path had no parent on a fresh host, so the non-recursive
  # `checkpath -d` failed, supervise-daemon could not open output_log/error_log, and
  # the babysitter never stayed up (landed in /run/openrc/failed/).
  local log_dir
  log_dir="$(tenant_lw_root "$name")/logs"

  cat >"$babysitter" <<INITEOF
#!/sbin/openrc-run

description="LunarWing ${type} container babysitter ($name)"

: "\${babysitter_container:=$container}"
: "\${babysitter_user:=$name}"
: "\${babysitter_home:=$home}"
: "\${babysitter_uid:=$uid}"
: "\${babysitter_respawn_delay:=2}"
: "\${babysitter_respawn_max:=10}"
: "\${babysitter_respawn_period:=120}"
: "\${babysitter_log_dir:=$log_dir}"
: "\${babysitter_output_log:=\${babysitter_log_dir}/${type}-babysitter.log}"
: "\${babysitter_error_log:=\${babysitter_log_dir}/${type}-babysitter.err}"

supervisor="supervise-daemon"
command="/usr/local/sbin/lunarwing-ctr-babysit"
command_args="\${babysitter_container}"
command_user="\${babysitter_user}:\${babysitter_user}"
respawn_delay="\${babysitter_respawn_delay}"
respawn_max="\${babysitter_respawn_max}"
respawn_period="\${babysitter_respawn_period}"
output_log="\${babysitter_output_log}"
error_log="\${babysitter_error_log}"
required_files="\${command}"

depend() {
    need net localmount
    after firewall
}

start_pre() {
    checkpath -d -m 0700 -o "\${babysitter_user}:\${babysitter_user}" "/run/user/\${babysitter_uid}"
    checkpath -d -m 0750 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_log_dir}"
    checkpath -f -m 0640 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_output_log}"
    checkpath -f -m 0640 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_error_log}"

    # Export rootless environment. Verified at runtime on OpenRC 0.63.1: across its
    # setuid, supervise-daemon re-sets HOME to the tenant's passwd home and leaves
    # XDG_RUNTIME_DIR untouched, so the supervised podman-wait process already runs
    # with HOME=/home/<t> + XDG_RUNTIME_DIR=/run/user/<uid>. These exports are belt-
    # and-suspenders; no sudo -u wrapper is needed. (No backticks in this heredoc:
    # it is unquoted, so backticks would be executed at render time.)
    if [ -n "\${babysitter_home}" ]; then
        export HOME="\${babysitter_home}"
    fi
    if [ -n "\${babysitter_uid}" ]; then
        export XDG_RUNTIME_DIR="/run/user/\${babysitter_uid}"
    fi
}
INITEOF
  chmod 0755 "$babysitter"
}

# Register a babysitter unit for a container (render + boot-enable + start).
# Idempotent: safe to call multiple times.
_register_babysitter() {
  local name="$1" type="$2" container="$3" uid="$4" home="$5"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  # render_container_babysitter_unit installs the helper itself (covers pg + workers).
  render_container_babysitter_unit "$name" "$type" "$container" "$uid" "$home"
  rc-update add "${container}-sup" default >/dev/null 2>&1 || true
  rc-service "${container}-sup" start >/dev/null 2>&1 || true
}

# Deregister a babysitter unit (stop + boot-disable + remove).
_deregister_babysitter() {
  local container="$1"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/${container}-sup" ]] || return 0
  rc-service "${container}-sup" stop >/dev/null 2>&1 || true
  rc-update del "${container}-sup" default >/dev/null 2>&1 || true
  rm -f "/etc/init.d/${container}-sup"
}

render_vision_openrc_unit() {
  local name="$1"
  ensure_container_runtime
  local vision_port runtime_bin container uid home
  vision_port="$(ports_get "$name" vision_service)"
  [[ -n "$vision_port" ]] || return 0
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  container="lunarwing-vision-${name}"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"

  cat >"/etc/init.d/${container}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing vision sidecar ($name)"

: "\${vis_runtime:=$runtime_bin}"
: "\${vis_container:=$container}"
: "\${vis_rootless:=$MT_ROOTLESS}"
: "\${vis_user:=$name}"
: "\${vis_home:=$home}"
: "\${vis_uid:=$uid}"
: "\${vis_port:=$vision_port}"
: "\${vis_internal_port:=$VISION_SIDECAR_INTERNAL_PORT}"
: "\${vis_health_internal_port:=$VISION_SIDECAR_HEALTH_PORT}"
: "\${vis_image:=$VISION_SIDECAR_IMAGE}"
: "\${vis_env_file:=$(tenant_env_dir "$name")/vision.env}"
: "\${vis_wait:=30}"

depend() {
    need net localmount
    after firewall lunarwing-${name}
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
    [ -n "\${vis_runtime}" ] && [ -x "\${vis_runtime}" ] || { ewarn "no container runtime; skipping vision for $name"; return 0; }
    ebegin "Starting vision sidecar (\${vis_container})"
    if [ "\${vis_rootless}" = "true" ]; then
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
    eend 0
}

status() {
    if _vis_healthy; then
        einfo "\${vis_container}: started"; return 0
    fi
    einfo "\${vis_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/${container}"
}

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
command_args="--env-file \${weechat_env_file} -- \${weechat_command} -L weechat-${name} new-session -d -s weechat '\${weechat_start_helper} --weechat-bin \${weechat_binary} --weechat-home \${weechat_home}'"
command_user="\${weechat_user}:\${weechat_group}"
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
        "\${weechat_start_helper} --weechat-bin \${weechat_binary} --weechat-home \${weechat_home}"
    eend \$?
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
    eend "\${stop_rc}"
    return "\${stop_rc}"
}
INITEOF
  chmod 0755 "$out_file"
}

# ── OpenRC service units ─────────────────────────────────────────────────────

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

render_tenant_openrc_units() {
  local name="$1"
  local repo env_dir state_dir log_dir run_dir
  repo="$(tenant_repo "$name")"
  env_dir="$(tenant_env_dir "$name")"
  state_dir="$(tenant_state_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  install_openrc_env_exec

  local proxy_port bridge_port weechat_port
  proxy_port="$(ports_get "$name" proxy)"
  bridge_port="$(ports_get "$name" bridge)"
  weechat_port="$(ports_get "$name" weechat)"

  local proxy_bin
  # Run from the tenant's own clone (in their home), not the admin's source repo,
  # so a tenant's services aren't coupled to another user's home directory.
  proxy_bin="$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"

  local ws_adapter_path
  ws_adapter_path="$(tenant_lw_root "$name")/lunarwing_weechat_wss/weechat_relay/ws_adapter.py"
  local ws_adapter_dir
  ws_adapter_dir="$(dirname "$ws_adapter_path")"

  local darkirc_rc_after=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_rc_after=" lunarwing-darkirc-adapter-${name}"
  fi

  local proxy_rc_after=""
  if tenant_proxy_enabled "$name"; then
    proxy_rc_after=" lunarwing-proxy-${name}"
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
  cat >"/etc/init.d/lunarwing-pg-${name}" <<INITEOF
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
  chmod 0755 "/etc/init.d/lunarwing-pg-${name}"
  render_container_babysitter_unit "$name" pg "$pg_container" "$pg_uid" "$pg_home"

  # ── Main daemon init script ──
  cat >"/etc/init.d/lunarwing-${name}" <<INITEOF
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

command="\${lunarwing_openrc_env_exec}"
command_args="--env-file \${lunarwing_env_file} -- \${lunarwing_command} \${lunarwing_args}"
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
    use dns logger
    after firewall lunarwing-pg-${name} xmpp-bridge-${name}${proxy_rc_after} weechat-${name} lunarwing-weechat-adapter-${name}${darkirc_rc_after}
}

start_pre() {
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
  chmod 0755 "/etc/init.d/lunarwing-${name}"

  # ── XMPP bridge init script ──
  cat >"/etc/init.d/xmpp-bridge-${name}" <<INITEOF
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
  chmod 0755 "/etc/init.d/xmpp-bridge-${name}"

  # ── DarkIRC daemon init script (only when enabled) ──
  if tenant_darkirc_enabled "$name"; then
  cat >"/etc/init.d/lunarwing-darkirc-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing DarkIRC daemon ($name)"

: "\${darkirc_command:=$DARKIRC_BIN}"
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
  chmod 0755 "/etc/init.d/lunarwing-darkirc-${name}"
  fi

  # ── TensorZero proxy init script (only when enabled) ──
  if tenant_proxy_enabled "$name"; then
  cat >"/etc/init.d/lunarwing-proxy-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing TensorZero proxy ($name)"

: "\${proxy_command:=$(command -v python3)}"
: "\${proxy_args:=$proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero $DEFAULT_TENSORZERO_URL}"
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
  chmod 0755 "/etc/init.d/lunarwing-proxy-${name}"
  fi

  # ── WeeChat init script (tmux-based) ──
  _render_weechat_openrc_unit "$name" "/etc/init.d/lunarwing-weechat-${name}"

  # WeeChat WS adapter init script
  cat >"/etc/init.d/lunarwing-weechat-adapter-${name}" <<INITEOF
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
command_args="--env-file \${adapter_env_file} -- \${adapter_command} \${adapter_args}"
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
  chmod 0755 "/etc/init.d/lunarwing-weechat-adapter-${name}"

  # DarkIRC adapter init script (only when enabled)
  if tenant_darkirc_enabled "$name"; then
  local darkirc_adapter_path darkirc_adapter_dir
  darkirc_adapter_path="$(tenant_lw_root "$name")/darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py"
  darkirc_adapter_dir="$(dirname "$darkirc_adapter_path")"

  cat >"/etc/init.d/lunarwing-darkirc-adapter-${name}" <<INITEOF
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
  chmod 0755 "/etc/init.d/lunarwing-darkirc-adapter-${name}"
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
  cat >"/etc/conf.d/lunarwing-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
lunarwing_rc_need="xmpp-bridge-${name}${proxy_rc_need} lunarwing-weechat-${name} lunarwing-weechat-adapter-${name}${darkirc_rc_need}"
CONFD

  cat >"/etc/conf.d/xmpp-bridge-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
xmpp_bridge_rc_before="lunarwing-${name}"
CONFD

  if tenant_proxy_enabled "$name"; then
    cat >"/etc/conf.d/lunarwing-proxy-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  cat >"/etc/conf.d/lunarwing-weechat-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD

  cat >"/etc/conf.d/lunarwing-weechat-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD

  if tenant_darkirc_enabled "$name"; then
    cat >"/etc/conf.d/lunarwing-darkirc-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  say "rendered OpenRC init scripts and conf.d for $name"
}

start_tenant_openrc() {
  local name="$1"
  # Record operator intent UP FRONT: enabling the primary daemon in the default
  # runlevel marks this tenant as "started" (boot-persistent) regardless of whether
  # any unit's FIRST start succeeds. health-openrc.sh's started-gate
  # (tenant_started) keys on exactly this, so a daemon that crashes on its first
  # start is still reported `critical` (a real outage) and remediated — not masked
  # as `skipped`. The selective rc-update-add loop below additionally boot-enables
  # the optional units that actually came up. Idempotent.
  rc-update add "lunarwing-${name}" default >/dev/null 2>&1 || true

  # Postgres first: the daemon `need`s it (and it's idempotent if already up).
  rc-service "lunarwing-pg-${name}" start
  # Start PG babysitter (supervises the container via podman wait)
  rc-service "lunarwing-pg-${name}-sup" start 2>/dev/null || true
  # Optional channels next, non-fatal: a missing weechat/aiohttp must not abort
  # the core stack (the main daemon does not depend on them).
  rc-service "lunarwing-weechat-${name}" start 2>/dev/null || say "  (lunarwing-weechat-${name} skipped — optional)"
  rc-service "lunarwing-weechat-adapter-${name}" start 2>/dev/null || say "  (lunarwing-weechat-adapter-${name} skipped — optional)"
  if tenant_darkirc_enabled "$name"; then
    rc-service "lunarwing-darkirc-${name}" start 2>/dev/null || say "  (lunarwing-darkirc-${name} skipped — optional)"
    rc-service "lunarwing-darkirc-adapter-${name}" start 2>/dev/null || say "  (lunarwing-darkirc-adapter-${name} skipped — optional)"
  fi
  if tenant_proxy_enabled "$name"; then
    rc-service "lunarwing-proxy-${name}" start
  fi
  rc-service "xmpp-bridge-${name}" start
  rc-service "lunarwing-${name}" start
  # Start worker babysitters (if workers are configured)
  for worker in nanocode pebble opencode; do
    rc-service "lunarwing-${worker}-${name}-sup" start 2>/dev/null || true
  done
  say "OpenRC services started for $name"

  # Auto-enable on boot whatever is actually running (idempotent, OpenRC only).
  local svc
  local boot_svcs=("lunarwing-pg-${name}" "lunarwing-pg-${name}-sup" "xmpp-bridge-${name}" "lunarwing-${name}" "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}" "lunarwing-vision-${name}")
  if tenant_proxy_enabled "$name"; then
    boot_svcs+=("lunarwing-proxy-${name}")
  fi
  if tenant_darkirc_enabled "$name"; then
    boot_svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
  fi
  for svc in "${boot_svcs[@]}"; do
    if rc-service "$svc" status >/dev/null 2>&1; then
      rc-update add "$svc" default >/dev/null 2>&1 || true
    fi
  done
  # Auto-enable worker babysitters
  for worker in nanocode pebble opencode; do
    if rc-service "lunarwing-${worker}-${name}-sup" status >/dev/null 2>&1; then
      rc-update add "lunarwing-${worker}-${name}-sup" default >/dev/null 2>&1 || true
    fi
  done
  say "enabled boot persistence (default runlevel) for $name's running services"
}

stop_tenant_openrc() {
  local name="$1"
  rc-service "lunarwing-${name}" stop 2>/dev/null || true
  rc-service "xmpp-bridge-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-proxy-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-weechat-adapter-${name}" stop 2>/dev/null || true
  if rc-service "lunarwing-weechat-${name}" status >/dev/null 2>&1; then
    if ! rc-service "lunarwing-weechat-${name}" stop; then
      say "failed to stop lunarwing-weechat-${name}; refusing to continue"
      return 1
    fi
  fi
  rc-service "lunarwing-darkirc-adapter-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-darkirc-${name}" stop 2>/dev/null || true
  # Worker babysitters: stop the supervisors so they don't respawn the stopped containers.
  for worker in nanocode pebble opencode; do
    rc-service "lunarwing-${worker}-${name}-sup" stop 2>/dev/null || true
  done
  rc-service "lunarwing-vision-${name}" stop 2>/dev/null || true
  # Postgres last: the daemon depends on it, so it stops after its consumers.
  rc-service "lunarwing-pg-${name}-sup" stop 2>/dev/null || true
  rc-service "lunarwing-pg-${name}" stop 2>/dev/null || true
  say "OpenRC services stopped for $name"
}

uninstall_tenant_openrc() {
  local name="$1"
  for svc in "lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}" "lunarwing-darkirc-adapter-${name}" "lunarwing-darkirc-${name}" "lunarwing-pg-${name}" "lunarwing-nanocode-${name}" "lunarwing-pebble-${name}" "lunarwing-opencode-${name}" "lunarwing-vision-${name}" "lunarwing-pg-${name}-sup" "lunarwing-nanocode-${name}-sup" "lunarwing-pebble-${name}-sup" "lunarwing-opencode-${name}-sup"; do
    rc-update del "$svc" default 2>/dev/null || true
    rm -f "/etc/init.d/$svc" "/etc/conf.d/$svc"
  done
  say "uninstalled OpenRC services for $name"
}

