#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2034  # state defaults here are consumed by sibling libraries/root
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 02-containers.sh — part of the lunarwing-mt-admin library.
#
# Container runtime helpers (_ctr, image prep, OpenRC podman healthcheck
# reconciliation), container babysitters, and the LunarVision/LunarVoice
# sidecar container lifecycles.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced.
#
# Extracted from the v2.0.8.0 monolith (goals item #9), keeping the library
# structure of ref2/sunburst/mtadmin-refactor-v2.0.6.0 per
# docs/proposals/MT-ADMIN-DECOMPOSITION.md. Pure line-range move: no function
# body was rewritten.


# Run the container runtime for a TENANT's containers. When rootless (podman),
# execute as the tenant user against their rootless store + runtime dir; when
# rootful (docker), run as root unchanged. Every per-tenant pg/worker container
# operation MUST go through this so inspect/start/stop/exec/rm hit the SAME store
# that owns the container — root and rootless podman are separate universes.
_ctr() {
  local name="$1"; shift
  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    local uid home
    uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
    home="$(getent passwd "$name" | cut -d: -f6)"
    # Run from a tenant-traversable CWD (F10): `sudo -u` keeps the caller's cwd, so
    # when mt-admin runs from an admin dir the tenant can't enter (e.g. ~dame, 0700)
    # `sudo -u` aborts with "cannot chdir ... Permission denied" BEFORE the runtime
    # runs — which silently broke the rootless pg readiness gate (it always timed
    # out). `/` is always traversable; no _ctr call passes a cwd-relative path. exec
    # preserves the exit code and the stdin/stdout redirects used by exec/pg_dump.
    ( cd / && exec sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" "$CONTAINER_RT" "$@" )
  else
    "$CONTAINER_RT" "$@"
  fi
}

# Podman delegates periodic healthcheck scheduling to systemd timers. On OpenRC,
# image-defined healthchecks therefore remain "starting" forever even while the
# generated OpenRC units' active endpoint probes report healthy. Disable only
# Podman's unscheduled copy; OpenRC remains the health authority.
_openrc_podman_uses_init_health() {
  [[ "${INIT_SYSTEM:-}" == "openrc" && "${CONTAINER_RT:-}" == "podman" ]]
}

# Remove a pre-fix container once so its inherited image healthcheck can be
# replaced by --no-healthcheck on the following run command.
_reconcile_openrc_podman_healthcheck() {  # <tenant> <container>
  local name="$1" container="$2" healthcheck
  _openrc_podman_uses_init_health || return 0
  healthcheck="$(_ctr "$name" inspect -f '{{json .Config.Healthcheck}}' "$container" 2>/dev/null || true)"
  case "$healthcheck" in
    ""|null|*'"NONE"'*) return 0 ;;
  esac
  say "OpenRC owns health for $container; recreating without unsupported Podman timer healthcheck"
  _ctr "$name" stop "$container" >/dev/null 2>&1 || true
  _ctr "$name" rm -f "$container" >/dev/null 2>&1 || true
}

# Ensure a worker image is available to whoever will run the tenant's container.
# Rootful (docker): the shared root store already has it — just verify presence.
# Rootless (podman): the image lives in the tenant's OWN store; if absent, copy it
# from the admin (root) store via save|load (per-tenant, ~minutes for large images;
# a shared additionalimagestore would avoid the N copies but isn't wired yet).
# Returns non-zero if the image can't be made available (caller should skip).
_ensure_tenant_image() {
  local name="$1" image="$2"

  # The admin (root) store's image ID is the source of truth for "current".
  local admin_id=""
  if "$CONTAINER_RT" image inspect -f '{{.Id}}' "$image" &>/dev/null; then
    admin_id="$("$CONTAINER_RT" image inspect -f '{{.Id}}' "$image")"
  fi

  # rootful: the admin store IS the runtime store, so presence there suffices.
  if [[ "$MT_ROOTLESS" != "true" ]]; then
    [[ -n "$admin_id" ]] && return 0
    return 1   # rootful + not built yet -> caller skips (build first)
  fi

  # rootless: each tenant has its own store. Skip only when the tenant already
  # holds the CURRENT image (same ID as the admin store) — not merely when the
  # name exists — so a rebuilt worker image actually reaches tenants instead of
  # being silently held back by a stale same-named copy.
  local tenant_id=""
  if _ctr "$name" image inspect -f '{{.Id}}' "$image" &>/dev/null; then
    tenant_id="$(_ctr "$name" image inspect -f '{{.Id}}' "$image")"
  fi
  if [[ -n "$tenant_id" && "$tenant_id" == "$admin_id" ]]; then
    return 0   # tenant already has the current image
  fi
  [[ -n "$admin_id" ]] || return 1   # rootless, but the admin store has no source image to copy

  if [[ -n "$tenant_id" ]]; then
    say "refreshing stale image $image in ${name}'s rootless store (save|load — minutes for large images) ..."
  else
    say "distributing image $image into ${name}'s rootless store (save|load — minutes for large images) ..."
  fi
  if "$CONTAINER_RT" save "$image" | _ctr "$name" load >/dev/null 2>&1; then
    # Drop the previous (now-untagged) image if the load re-pointed the tag, so
    # repeated worker-image updates don't accumulate GBs of stale layers in the
    # tenant's rootless store.
    _ctr "$name" image prune -f >/dev/null 2>&1 || true
    say "image $image available in ${name}'s store"
    return 0
  fi
  say "WARNING: failed to load $image into ${name}'s store"
  return 1
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

# ── LunarVision OCR/vision sidecar ──────────────────────────────────────────

VISION_SIDECAR_IMAGE=lunarwing/vision-service:latest
VISION_SIDECAR_INTERNAL_PORT=8088
VISION_SIDECAR_HEALTH_PORT=8089

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

_voice_env_set_default() {
  local path="$1" key="$2" value="$3"
  [[ -n "$(_env_existing "$path" "$key")" ]] || _env_set "$path" "$key" "$value"
}

write_tenant_voice_env() {
  local name="$1" env_dir env_path token enabled setting key value
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/voice.env"
  mkdir -p "$env_dir"
  if [[ ! -e "$env_path" ]]; then
    (umask 077; : >"$env_path")
  elif [[ ! -f "$env_path" || -L "$env_path" ]]; then
    die "unsafe LunarVoice environment file: $env_path"
  fi

  token="$(_env_existing "$env_path" VOICE_AUTH_TOKEN)"
  token="${token:-$(generate_token)}"
  enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_voice // false' "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$enabled" == true ]] || enabled=false

  _env_set "$env_path" VOICE_ENABLED "$enabled"
  _voice_env_set_default "$env_path" VOICE_AUTH_TOKEN "$token"
  _env_set "$env_path" VOICE_ALLOW_UNAUTHENTICATED false
  _env_set "$env_path" VOICE_BIND_HOST 0.0.0.0
  _env_set "$env_path" VOICE_PORT "$VOICE_SIDECAR_INTERNAL_PORT"
  _env_set "$env_path" VOICE_HEALTH_PORT "$VOICE_SIDECAR_HEALTH_PORT"
  _env_set "$env_path" VOICE_MODEL_DIR /models
  _env_set "$env_path" VOICE_AUTO_DOWNLOAD false
  _env_set "$env_path" VOICE_REQUIRE_STT true

  for setting in \
    'VOICE_STT_ENGINE|whisper' \
    'VOICE_STT_MODEL|whisper-tiny' \
    'VOICE_STT_LANGUAGE|auto' \
    'VOICE_ENABLE_VAD|true' \
    'VOICE_ENABLE_TTS|true' \
    'VOICE_MAX_AUDIO_SECS|300' \
    'VOICE_MAX_AUDIO_BYTES|52428800' \
    'VOICE_MAX_TEXT_CHARS|10000' \
    'VOICE_MAX_TTS_AUDIO_SECS|300' \
    'VOICE_REQUEST_TIMEOUT_SECS|120' \
    'VOICE_MAX_CONCURRENT_INFERENCE|2' \
    'VOICE_RATE_LIMIT_PER_SEC|5' \
    'VOICE_CACHE_TTL_SECS|600' \
    'VOICE_CACHE_MAX_ENTRIES|5000' \
    'VOICE_FFMPEG_PATH|ffmpeg' \
    'VOICE_TTS_DEFAULT_VOICE|af_heart' \
    'ORT_NUM_THREADS|4'; do
    key="${setting%%|*}"
    value="${setting#*|}"
    _voice_env_set_default "$env_path" "$key" "$value"
  done
  chmod 0600 "$env_path"
  chown "$name:$name" "$env_path"
  printf '%s' "$token"
}

_voice_model_path_safe() {
  local value="$1"
  [[ "$value" == /models/* \
    && "$value" != *'/../'* && "$value" != */.. \
    && "$value" != *'/./'* && "$value" != */. ]]
}

_voice_models_ready_env() {
  local env_path="$1" key value default_path host_path host_real
  local stt_model stt_engine marker marker_hash model_paths model_root model_size
  stt_model="$(_env_existing "$env_path" VOICE_STT_MODEL)"
  stt_model="${stt_model:-whisper-tiny}"
  stt_engine="$(_env_existing "$env_path" VOICE_STT_ENGINE)"
  stt_engine="${stt_engine:-whisper}"
  [[ "$stt_model" =~ ^[A-Za-z0-9._-]+$ ]] || return 1
  marker="$VOICE_MODELS_DIR/.lunarvoice-${stt_model}.sha256"
  [[ -f "$marker" && ! -L "$marker" ]] || return 1
  marker_hash="$(tr -d '\r\n' <"$marker")"
  [[ "$marker_hash" =~ ^[0-9a-f]{64}$ ]] || return 1
  model_root="$(realpath -e -- "$VOICE_MODELS_DIR" 2>/dev/null)" || return 1

  case "$stt_engine" in
    whisper)
      model_size="${stt_model#whisper-}"
      [[ -n "$model_size" && "$model_size" != "$stt_model" ]] || return 1
      model_paths="VOICE_STT_ENCODER|/models/sherpa-onnx-${stt_model}/${model_size}-encoder.int8.onnx
VOICE_STT_DECODER|/models/sherpa-onnx-${stt_model}/${model_size}-decoder.int8.onnx
VOICE_STT_TOKENS|/models/sherpa-onnx-${stt_model}/${model_size}-tokens.txt"
      ;;
    parakeet|parakeet-tdt|transducer)
      model_paths="VOICE_STT_ENCODER|/models/sherpa-onnx-nemo-${stt_model}/encoder.int8.onnx
VOICE_STT_DECODER|/models/sherpa-onnx-nemo-${stt_model}/decoder.int8.onnx
VOICE_STT_JOINER|/models/sherpa-onnx-nemo-${stt_model}/joiner.int8.onnx
VOICE_STT_TOKENS|/models/sherpa-onnx-nemo-${stt_model}/tokens.txt"
      ;;
    *) return 1 ;;
  esac

  while IFS='|' read -r key default_path; do
    value="$(_env_existing "$env_path" "$key")"
    value="${value:-$default_path}"
    _voice_model_path_safe "$value" || return 1
    host_path="$VOICE_MODELS_DIR/${value#/models/}"
    [[ -f "$host_path" && -r "$host_path" && ! -L "$host_path" ]] || return 1
    host_real="$(realpath -e -- "$host_path" 2>/dev/null)" || return 1
    [[ "$host_real" == "$model_root"/* ]] || return 1
  done <<<"$model_paths"
}

voice_models_ready() {
  _voice_models_ready_env "$(tenant_env_dir "$1")/voice.env"
}

_normalize_external_vision_url() {
  local url="${1%/}" authority host port=""
  [[ "$url" == https://* && "$url" != *[[:space:]]* ]] || return 1
  authority="${url#https://}"
  [[ -n "$authority" && "$authority" != *['/?#@']* ]] || return 1

  if [[ "$authority" == \[* ]]; then
    [[ "$authority" =~ ^\[([0-9A-Fa-f:.]+)\](:([0-9]+))?$ ]] || return 1
    host="${BASH_REMATCH[1]}"
    port="${BASH_REMATCH[3]:-}"
    [[ "$host" == *:* ]] || return 1
  else
    [[ "$authority" != *:*:* ]] || return 1
    if [[ "$authority" == *:* ]]; then
      host="${authority%:*}"
      port="${authority##*:}"
    else
      host="$authority"
    fi
    [[ "$host" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] || return 1
    [[ "$host" != *..* ]] || return 1
  fi
  [[ -z "$port" ]] || _vision_port_valid "$port" || return 1
  printf '%s' "$url"
}

_external_vision_endpoint_healthy() {
  local base_url="$1" response
  response="$(curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
    --connect-timeout 5 --max-time 10 "${base_url}/health" 2>/dev/null)" || return 1
  jq -e '.status == "ok"' <<<"$response" >/dev/null 2>&1
}

_wait_external_vision_healthy() {
  local base_url="$1" attempts="${2:-6}" attempt
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    _external_vision_endpoint_healthy "$base_url" && return 0
    (( attempt < attempts )) && sleep 2
  done
  return 1
}

_vision_daemon_running() {
  local name="$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service" >/dev/null 2>&1
  else
    rc-service "lunarwing-${name}" status >/dev/null 2>&1
  fi
}

_reload_vision_consumer_if_running() {
  local name="$1"
  if _vision_daemon_running "$name"; then
    _restart_tenant_daemon "$name" || {
      say "ERROR: failed to restart LunarWing after vision configuration change" >&2
      return 1
    }
    say "LunarWing restarted; vision configuration is active for $name"
  else
    say "LunarWing is stopped; vision configuration applies on its next start"
  fi
}

_set_vision_daemon_endpoint() {
  local name="$1" service_url="$2" token="$3"
  local env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" && ! -L "$env_path" ]] \
    || { say "ERROR: tenant LunarWing environment missing or unsafe" >&2; return 1; }
  [[ -n "$token" && "$token" != *$'\n'* && "$token" != *$'\r'* ]] \
    || { say "ERROR: LunarVision token must contain exactly one non-empty line" >&2; return 1; }
  _env_set "$env_path" VISION_SERVICE_URL "$service_url"
  _env_set "$env_path" VISION_AUTH_TOKEN "$(_quote_env_value "$token")"
}

_vision_port_valid() {
  local port="${1:-}"
  [[ "$port" =~ ^[0-9]+$ ]] && (( 10#$port > 0 && 10#$port <= 65535 ))
}

_vision_launch_contract() {
  local name="$1" vision_port="$2" health_port="$3" env_path="$4" image_id env_hash
  image_id="$(_ctr "$name" image inspect -f '{{.Id}}' "$VISION_SIDECAR_IMAGE" 2>/dev/null)" || return 1
  env_hash="$(_container_config_hash "$env_path")" || return 1
  [[ -n "$image_id" && -n "$env_hash" ]] || return 1
  printf 'image=%s;env=%s;api=%s:%s;health=%s:%s;runtime=%s;rootless=%s;init=%s;no_new_privileges=true' \
    "$image_id" "$env_hash" "$vision_port" "$VISION_SIDECAR_INTERNAL_PORT" \
    "$health_port" "$VISION_SIDECAR_HEALTH_PORT" "$CONTAINER_RT" \
    "$MT_ROOTLESS" "$INIT_SYSTEM"
}

_vision_container_healthy() {
  local name="$1" container="$2" running
  running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" || return 1
  [[ "$running" == "true" ]] || return 1
  _ctr "$name" exec "$container" curl -sf -o /dev/null --max-time 3 \
    "http://127.0.0.1:${VISION_SIDECAR_HEALTH_PORT}/health" 2>/dev/null
}

_wait_tenant_vision_healthy() {
  local name="$1" container="$2" attempts="${3:-30}" attempt
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    _vision_container_healthy "$name" "$container" && return 0
    (( attempt < attempts )) && sleep 1
  done
  return 1
}

_stop_vision_openrc_supervisor() {
  local container="$1" supervisor="${1}-sup"
  [[ "$INIT_SYSTEM" == "openrc" && -f "$OPENRC_INITD_DIR/$supervisor" ]] || return 0
  rc-service "$supervisor" stop >/dev/null 2>&1 && return 0
  rc-service "$supervisor" status 2>&1 | grep -Eqi 'stopped|inactive|dead'
}

_create_vision_container() {
  local name="$1" container="$2" env_path="$3" vision_port="$4" health_port="$5"
  local -a healthcheck_args=() restart_arg=()
  _openrc_podman_uses_init_health && healthcheck_args=(--no-healthcheck)
  [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
  say "creating vision sidecar container $container on ports $vision_port/$health_port"
  _ctr "$name" run -d \
    --name "$container" \
    "${healthcheck_args[@]}" \
    --env-file "$env_path" \
    -p "127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}" \
    -p "127.0.0.1:${health_port}:${VISION_SIDECAR_HEALTH_PORT}" \
    --security-opt=no-new-privileges \
    "${restart_arg[@]}" \
    "$VISION_SIDECAR_IMAGE" >/dev/null
}

_vision_has_unsupported_openrc_healthcheck() {
  local name="$1" container="$2" healthcheck
  _openrc_podman_uses_init_health || return 1
  healthcheck="$(_ctr "$name" inspect -f '{{json .Config.Healthcheck}}' "$container" 2>/dev/null || true)"
  case "$healthcheck" in
    ""|null|*'"NONE"'*) return 1 ;;
    *) return 0 ;;
  esac
}

_stop_vision_supervision() {
  local name="$1" container="$2" path
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    _stop_vision_openrc_supervisor "$container"
    return
  fi
  [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" \
    && "$CONTAINER_RT" == "podman" ]] || return 0
  path="$(tenant_home "$name")/.config/systemd/user/${container}.service"
  [[ ! -e "$path" && ! -L "$path" ]] && return 0
  if [[ ! -f "$path" || -L "$path" ]] \
      || ! grep -q '^# Managed by lunarwing-mt-admin: LunarVision fallback$' "$path"; then
    say "ERROR: refusing to stop unmanaged systemd unit $path" >&2
    return 1
  fi
  _systemctl_user "$name" stop "${container}.service" >/dev/null 2>&1
}

_reconcile_vision_container() {
  local name="$1" container="$2" env_path="$3" contract="$4" recreate=false
  if _ctr "$name" inspect "$container" &>/dev/null; then
    if _vision_has_unsupported_openrc_healthcheck "$name" "$container" \
        || _container_config_changed "$name" "$container" "$env_path" "$contract"; then
      recreate=true
    fi
  fi

  if [[ "$recreate" == "true" ]]; then
    say "config, image, or health ownership changed for $container; force-recreating"
    _stop_vision_supervision "$name" "$container" || return 1
    if [[ "$INIT_SYSTEM" == "openrc" && -f "$OPENRC_INITD_DIR/$container" ]]; then
      rc-service "$container" stop >/dev/null 2>&1 || true
    fi
    _ctr "$name" stop --time 30 "$container" >/dev/null 2>&1 || true
    _ctr "$name" rm -f "$container" >/dev/null || return 1
  fi

  if ! _ctr "$name" inspect "$container" &>/dev/null; then
    _create_vision_container "$name" "$container" "$env_path" \
      "$(ports_get "$name" vision_service)" "$(ports_get "$name" vision_health)" || return 1
  elif ! _vision_container_healthy "$name" "$container"; then
    _stop_vision_supervision "$name" "$container" || return 1
    _ctr "$name" stop --time 30 "$container" >/dev/null 2>&1 || true
    _ctr "$name" start "$container" >/dev/null || return 1
  fi

  if ! _wait_tenant_vision_healthy "$name" "$container"; then
    say "ERROR: vision sidecar $container did not become healthy" >&2
    return 1
  fi
  _store_container_hash "$name" "$container" "$env_path" "$contract"
}

start_tenant_vision() {
  local name="$1" vision_port health_port env_path container contract quadlet_file external_url
  if tenant_vision_external "$name"; then
    external_url="$(tenant_vision_external_url "$name")"
    external_url="$(_normalize_external_vision_url "$external_url")" || {
      say "ERROR: invalid external LunarVision URL for $name" >&2
      return 1
    }
    if ! _wait_external_vision_healthy "$external_url"; then
      say "ERROR: external LunarVision did not become healthy: $external_url" >&2
      return 1
    fi
    say "external LunarVision ready for $name ($external_url)"
    return 0
  fi

  ensure_container_runtime
  ensure_init_system
  vision_port="$(ports_get "$name" vision_service)" || true
  health_port="$(ports_get "$name" vision_health)" || true
  if ! _vision_port_valid "$vision_port" || ! _vision_port_valid "$health_port" \
      || [[ "$vision_port" == "$health_port" ]]; then
    say "ERROR: distinct valid vision_service and vision_health ports are required for $name" >&2
    return 1
  fi
  if ! _ensure_tenant_image "$name" "$VISION_SIDECAR_IMAGE"; then
    say "ERROR: vision sidecar image unavailable; run 'build-vision-sidecar'" >&2
    return 1
  fi
  env_path="$(tenant_env_dir "$name")/vision.env"
  [[ -f "$env_path" ]] || write_tenant_vision_env "$name" >/dev/null
  container="lunarwing-vision-$name"
  contract="$(_vision_launch_contract "$name" "$vision_port" "$health_port" "$env_path")" || return 1

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    _remove_vision_systemd_fallback "$name" || return 1
    quadlet_file="$(tenant_quadlet_dir "$name")/${container}.container"
    render_vision_quadlet "$name" || return 1
    _systemctl_user "$name" daemon-reload >/dev/null 2>&1 || return 1
    if _container_config_changed "$name" "$container" "$quadlet_file" "$contract"; then
      _recreate_quadlet_container "$name" "${container}.service" "$container"
    fi
    _systemctl_user "$name" reset-failed "${container}.service" >/dev/null 2>&1 || true
    if ! _systemctl_user "$name" start "${container}.service" >/dev/null 2>&1 \
        || ! _wait_tenant_vision_healthy "$name" "$container"; then
      say "ERROR: ${container}.service failed to become healthy" >&2
      _systemctl_user "$name" status "${container}.service" --no-pager --full >&2 || true
      return 1
    fi
    _store_container_hash "$name" "$container" "$quadlet_file" "$contract"
    say "vision sidecar ready via Quadlet ($container, ports $vision_port/$health_port)"
    return 0
  fi

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" \
      && "$CONTAINER_RT" == "podman" ]]; then
    _prepare_vision_systemd_fallback "$name" || return 1
  fi
  _reconcile_vision_container "$name" "$container" "$env_path" "$contract" || return 1
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    _register_vision_openrc "$name" || return 1
  elif [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]]; then
    _register_vision_systemd_fallback "$name" || return 1
  fi
  say "vision sidecar ready ($container, ports $vision_port/$health_port)"
}

stop_tenant_vision() {
  local name="$1" container="lunarwing-vision-${1}" running
  if tenant_vision_external "$name"; then
    say "external LunarVision remains operator-managed for $name"
    return 0
  fi
  ensure_container_runtime
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    _stop_vision_openrc_supervisor "$container" || return 1
    if [[ -f "$OPENRC_INITD_DIR/$container" ]]; then
      rc-service "$container" stop >/dev/null 2>&1 || true
    fi
  else
    _systemctl_user "$name" stop "${container}.service" >/dev/null 2>&1 || true
  fi
  if _ctr "$name" inspect "$container" &>/dev/null; then
    _ctr "$name" stop --time 30 "$container" >/dev/null 2>&1 || true
    running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null || true)"
    if [[ "$running" == "true" ]]; then
      say "ERROR: vision sidecar remained running after stop ($container)" >&2
      return 1
    fi
  fi
  say "vision sidecar stopped ($container)"
}

_retire_local_vision_runtime() {
  local name="$1" container="lunarwing-vision-${1}"
  ensure_container_runtime
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    remove_vision_systemd_assets "$name" || return 1
  else
    remove_vision_openrc_assets "$name" || return 1
  fi
  if _ctr "$name" inspect "$container" >/dev/null 2>&1; then
    _ctr "$name" stop --time 30 "$container" >/dev/null 2>&1 || true
    _ctr "$name" rm -f "$container" >/dev/null || return 1
  fi
}

_restore_vision_configuration() {
  local name="$1" backup="$2" previous_mode="$3" previous_url="$4"
  local env_path="$(tenant_env_dir "$name")/lunarwing.env"
  mv -T -- "$backup" "$env_path" || return 1
  ports_set_vision_mode "$name" "$previous_mode" "$previous_url"
  if [[ "$previous_mode" == local ]]; then
    start_tenant_vision "$name" || return 1
  fi
  _reload_vision_consumer_if_running "$name"
}

configure_tenant_vision_external() {
  local name="$1" requested_url="$2" token="$3" external_url
  local env_path backup previous_mode previous_url result=0
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  external_url="$(_normalize_external_vision_url "$requested_url")" \
    || die "external LunarVision URL must be an HTTPS origin without credentials, path, query, or fragment"
  [[ -n "$token" && "$token" != *$'\n'* && "$token" != *$'\r'* ]] \
    || die "external LunarVision token must contain exactly one non-empty line"
  _wait_external_vision_healthy "$external_url" \
    || die "external LunarVision health check failed: $external_url/health"

  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" && ! -L "$env_path" ]] \
    || die "tenant LunarWing environment missing or unsafe"
  backup="$(mktemp "$(dirname "$env_path")/.vision-rollback.XXXXXX")"
  cp --preserve=mode,ownership -- "$env_path" "$backup" \
    || { rm -f -- "$backup"; return 1; }
  previous_mode="$(tenant_vision_mode "$name")"
  previous_url="$(tenant_vision_external_url "$name")"

  _retire_local_vision_runtime "$name" || result=$?
  if [[ "$result" -eq 0 ]]; then
    _set_vision_daemon_endpoint "$name" "$external_url" "$token" || result=$?
  fi
  if [[ "$result" -eq 0 ]]; then
    ports_set_vision_mode "$name" external "$external_url"
    _reload_vision_consumer_if_running "$name" || result=$?
  fi
  if [[ "$result" -eq 0 ]]; then
    rm -f -- "$backup"
    say "LunarVision for $name now uses external instance $external_url"
    return 0
  fi

  say "ERROR: external LunarVision configuration failed; restoring previous settings" >&2
  _restore_vision_configuration "$name" "$backup" "$previous_mode" "$previous_url" \
    || say "WARNING: restart tenant '$name' to restore its prior LunarVision settings" >&2
  return "$result"
}

configure_tenant_vision_local() {
  local name="$1" env_path vision_env backup previous_mode previous_url token vision_port result=0
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  ensure_container_runtime
  if ! "$CONTAINER_RT" image inspect "$VISION_SIDECAR_IMAGE" >/dev/null 2>&1; then
    build_vision_sidecar_image || die "failed to build local LunarVision image"
  fi
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  vision_env="$(tenant_env_dir "$name")/vision.env"
  [[ -f "$env_path" && ! -L "$env_path" ]] \
    || die "tenant LunarWing environment missing or unsafe"
  [[ -f "$vision_env" && ! -L "$vision_env" ]] || write_tenant_vision_env "$name" >/dev/null
  token="$(_env_existing "$vision_env" LUNARWING_AUTH_TOKEN)"
  [[ -n "$token" ]] || die "local LunarVision token is missing"
  vision_port="$(ports_get "$name" vision_service)" || true
  _vision_port_valid "$vision_port" || die "local LunarVision service port is invalid"

  backup="$(mktemp "$(dirname "$env_path")/.vision-rollback.XXXXXX")"
  cp --preserve=mode,ownership -- "$env_path" "$backup" \
    || { rm -f -- "$backup"; return 1; }
  previous_mode="$(tenant_vision_mode "$name")"
  previous_url="$(tenant_vision_external_url "$name")"
  _set_vision_daemon_endpoint "$name" "http://127.0.0.1:$vision_port" "$token"
  ports_set_vision_mode "$name" local
  start_tenant_vision "$name" || result=$?
  if [[ "$result" -eq 0 ]]; then
    _reload_vision_consumer_if_running "$name" || result=$?
  fi
  if [[ "$result" -eq 0 ]]; then
    rm -f -- "$backup"
    say "LunarVision for $name now uses its local sidecar"
    return 0
  fi

  say "ERROR: local LunarVision configuration failed; restoring previous settings" >&2
  _retire_local_vision_runtime "$name" || true
  _restore_vision_configuration "$name" "$backup" "$previous_mode" "$previous_url" \
    || say "WARNING: restart tenant '$name' to restore its prior LunarVision settings" >&2
  return "$result"
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

_voice_port_valid() {
  local port="${1:-}"
  [[ "$port" =~ ^[0-9]+$ ]] && (( 10#$port > 0 && 10#$port <= 65535 ))
}

# Launch contract for the voice sidecar: captures every input that must force a
# container recreate when it drifts (image, env, port mapping, model store,
# runtime/init shape) but is not visible in a rendered unit file.
_voice_launch_contract() {
  local name="$1" voice_port="$2" health_port="$3" env_path="$4" image_id env_hash
  image_id="$(_ctr "$name" image inspect -f '{{.Id}}' "$VOICE_SIDECAR_IMAGE" 2>/dev/null)" || return 1
  env_hash="$(_container_config_hash "$env_path")" || return 1
  [[ -n "$image_id" && -n "$env_hash" ]] || return 1
  printf 'image=%s;env=%s;api=%s:%s;health=%s:%s;models=%s;runtime=%s;rootless=%s;init=%s;no_new_privileges=true' \
    "$image_id" "$env_hash" "$voice_port" "$VOICE_SIDECAR_INTERNAL_PORT" \
    "$health_port" "$VOICE_SIDECAR_HEALTH_PORT" "$VOICE_MODELS_DIR" "$CONTAINER_RT" \
    "$MT_ROOTLESS" "$INIT_SYSTEM"
}

_voice_container_healthy() {
  local name="$1" container="$2" running health_base
  running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" || return 1
  [[ "$running" == "true" ]] || return 1
  health_base="http://127.0.0.1:${VOICE_SIDECAR_HEALTH_PORT}"
  _ctr "$name" exec "$container" curl -sf -o /dev/null --max-time 3 \
    "$health_base/health" 2>/dev/null \
    && _ctr "$name" exec "$container" curl -sf -o /dev/null --max-time 3 \
      "$health_base/ready" 2>/dev/null
}

# STT model load can take a while on a cold box, so the default wait is 60s.
_wait_tenant_voice_healthy() {
  local name="$1" container="$2" attempts="${3:-60}" attempt
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    _voice_container_healthy "$name" "$container" && return 0
    (( attempt < attempts )) && sleep 1
  done
  return 1
}

_stop_voice_openrc_supervisor() {
  local container="$1" supervisor="${1}-sup"
  [[ "$INIT_SYSTEM" == "openrc" && -f "$OPENRC_INITD_DIR/$supervisor" ]] || return 0
  rc-service "$supervisor" stop >/dev/null 2>&1 && return 0
  rc-service "$supervisor" status 2>&1 | grep -Eqi 'stopped|inactive|dead'
}

_create_voice_container() {
  local name="$1" container="$2" env_path="$3" voice_port="$4" health_port="$5"
  local -a healthcheck_args=() restart_arg=()
  _openrc_podman_uses_init_health && healthcheck_args=(--no-healthcheck)
  [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
  say "creating voice sidecar container $container on ports $voice_port/$health_port"
  _ctr "$name" run -d \
    --name "$container" \
    "${healthcheck_args[@]}" \
    --env-file "$env_path" \
    -p "127.0.0.1:${voice_port}:${VOICE_SIDECAR_INTERNAL_PORT}" \
    -p "127.0.0.1:${health_port}:${VOICE_SIDECAR_HEALTH_PORT}" \
    -v "$VOICE_MODELS_DIR:/models:ro" \
    --security-opt=no-new-privileges \
    "${restart_arg[@]}" \
    "$VOICE_SIDECAR_IMAGE" >/dev/null
}

_voice_has_unsupported_openrc_healthcheck() {
  local name="$1" container="$2" healthcheck
  _openrc_podman_uses_init_health || return 1
  healthcheck="$(_ctr "$name" inspect -f '{{json .Config.Healthcheck}}' "$container" 2>/dev/null || true)"
  case "$healthcheck" in
    ""|null|*'"NONE"'*) return 1 ;;
    *) return 0 ;;
  esac
}

# Stop whichever supervision owns the voice container before mutating it, or the
# supervisor respawns the container mid-mutation. Refuses to touch a systemd
# fallback unit that was not rendered by mt-admin.
_stop_voice_supervision() {
  local name="$1" container="$2" path
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    _stop_voice_openrc_supervisor "$container"
    return
  fi
  [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" \
    && "$CONTAINER_RT" == "podman" ]] || return 0
  path="$(tenant_home "$name")/.config/systemd/user/${container}.service"
  [[ ! -e "$path" && ! -L "$path" ]] && return 0
  if [[ ! -f "$path" || -L "$path" ]] \
      || ! grep -q '^# Managed by lunarwing-mt-admin: LunarVoice fallback$' "$path"; then
    say "ERROR: refusing to stop unmanaged systemd unit $path" >&2
    return 1
  fi
  _systemctl_user "$name" stop "${container}.service" >/dev/null 2>&1
}

# Converge the voice container toward the current launch contract: recreate on
# config/image/health-ownership drift, restart when unhealthy, and fail loudly
# when the sidecar never becomes healthy. Idempotent when nothing drifted.
_reconcile_voice_container() {
  local name="$1" container="$2" env_path="$3" contract="$4" recreate=false
  if _ctr "$name" inspect "$container" &>/dev/null; then
    if _voice_has_unsupported_openrc_healthcheck "$name" "$container" \
        || _container_config_changed "$name" "$container" "$env_path" "$contract"; then
      recreate=true
    fi
  fi

  if [[ "$recreate" == "true" ]]; then
    say "config, image, or health ownership changed for $container; force-recreating"
    _stop_voice_supervision "$name" "$container" || return 1
    if [[ "$INIT_SYSTEM" == "openrc" && -f "$OPENRC_INITD_DIR/$container" ]]; then
      rc-service "$container" stop >/dev/null 2>&1 || true
    fi
    _ctr "$name" stop --time 30 "$container" >/dev/null 2>&1 || true
    _ctr "$name" rm -f "$container" >/dev/null || return 1
  fi

  if ! _ctr "$name" inspect "$container" &>/dev/null; then
    _create_voice_container "$name" "$container" "$env_path" \
      "$(ports_get "$name" voice_service)" "$(ports_get "$name" voice_health)" || return 1
  elif ! _voice_container_healthy "$name" "$container"; then
    _stop_voice_supervision "$name" "$container" || return 1
    _ctr "$name" stop --time 30 "$container" >/dev/null 2>&1 || true
    _ctr "$name" start "$container" >/dev/null || return 1
  fi

  if ! _wait_tenant_voice_healthy "$name" "$container"; then
    say "ERROR: voice sidecar $container did not become healthy" >&2
    return 1
  fi
  _store_container_hash "$name" "$container" "$env_path" "$contract"
}

start_tenant_voice() {
  local name="$1"
  tenant_voice_enabled "$name" || { say "LunarVoice disabled for $name (skipping)"; return 0; }
  voice_models_ready "$name" \
    || { say "ERROR: verified STT models missing under $VOICE_MODELS_DIR" >&2; return 1; }
  ensure_container_runtime
  ensure_init_system

  # SELinux: the shared model store must carry a container-readable label or
  # the sidecar gets EACCES on /models. chcon (not :z) because the dir is
  # root-owned and shared across tenants — a per-tenant relabel would fight.
  if command -v getenforce >/dev/null 2>&1 && [[ "$(getenforce)" == "Enforcing" ]]; then
    chcon -Rt container_file_t "$VOICE_MODELS_DIR" 2>/dev/null || true
  fi

  local voice_port voice_health_port container_name env_path contract quadlet_file
  voice_port="$(ports_get "$name" voice_service)" || true
  voice_health_port="$(ports_get "$name" voice_health)" || true
  if ! _voice_port_valid "$voice_port" || ! _voice_port_valid "$voice_health_port" \
      || [[ "$voice_port" == "$voice_health_port" ]]; then
    say "ERROR: distinct valid voice_service and voice_health ports are required for $name" >&2
    return 1
  fi
  if ! _ensure_tenant_image "$name" "$VOICE_SIDECAR_IMAGE"; then
    say "ERROR: LunarVoice image unavailable; run 'build-voice-sidecar' first" >&2
    return 1
  fi
  container_name="lunarwing-voice-$name"
  env_path="$(tenant_env_dir "$name")/voice.env"
  [[ -f "$env_path" ]] || write_tenant_voice_env "$name" >/dev/null
  contract="$(_voice_launch_contract "$name" "$voice_port" "$voice_health_port" "$env_path")" || return 1

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    _remove_voice_systemd_fallback "$name" || return 1
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-voice-${name}.container"
    render_voice_quadlet "$name" || return 1
    _systemctl_user "$name" daemon-reload >/dev/null 2>&1 || return 1
    if _container_config_changed "$name" "$container_name" "$quadlet_file" "$contract"; then
      _recreate_quadlet_container "$name" "${container_name}.service" "$container_name"
    fi
    _systemctl_user "$name" reset-failed "${container_name}.service" >/dev/null 2>&1 || true
    if ! _systemctl_user "$name" start "${container_name}.service" >/dev/null 2>&1 \
        || ! _wait_tenant_voice_healthy "$name" "$container_name"; then
      say "ERROR: ${container_name}.service failed to become healthy" >&2
      _systemctl_user "$name" status "${container_name}.service" --no-pager --full >&2 || true
      return 1
    fi
    _store_container_hash "$name" "$container_name" "$quadlet_file" "$contract"
    say "LunarVoice ready via Quadlet (${container_name}.service, ports $voice_port/$voice_health_port)"
    return 0
  fi

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" \
      && "$CONTAINER_RT" == "podman" ]]; then
    _prepare_voice_systemd_fallback "$name" || return 1
  fi
  _reconcile_voice_container "$name" "$container_name" "$env_path" "$contract" || return 1
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    _register_voice_openrc "$name" || return 1
  elif [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]]; then
    _register_voice_systemd_fallback "$name" || return 1
  fi
  say "LunarVoice ready ($container_name, ports $voice_port/$voice_health_port)"
}

stop_tenant_voice() {
  local name="$1" container_name="lunarwing-voice-$1" running
  ensure_container_runtime
  ensure_init_system
  # Supervision stops before the container or it respawns mid-stop.
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    _stop_voice_openrc_supervisor "$container_name" || return 1
    if [[ -f "$OPENRC_INITD_DIR/${container_name}" ]]; then
      rc-service "$container_name" stop >/dev/null 2>&1 || true
    fi
  else
    _systemctl_user "$name" stop "${container_name}.service" >/dev/null 2>&1 || true
  fi
  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop --time 30 "$container_name" >/dev/null 2>&1 || true
    running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null || true)"
    if [[ "$running" == "true" ]]; then
      say "ERROR: voice sidecar remained running after stop ($container_name)" >&2
      return 1
    fi
  fi
  say "LunarVoice stopped ($container_name)"
}

_voice_capture_transcription_config() {
  local voice_env="$1" lunarwing_env="$2" provider
  [[ "$(_env_existing "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_CAPTURED)" == true ]] && return 0
  provider="$(_env_existing "$lunarwing_env" TRANSCRIPTION_PROVIDER)"
  [[ "$provider" != lunarvoice ]] || return 0
  _env_set "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_ENABLED \
    "$(_env_existing "$lunarwing_env" TRANSCRIPTION_ENABLED)"
  _env_set "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_PROVIDER "$provider"
  _env_set "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_MODEL \
    "$(_env_existing "$lunarwing_env" TRANSCRIPTION_MODEL)"
  _env_set "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_CAPTURED true
}

_voice_restore_transcription_config() {
  local voice_env="$1" lunarwing_env="$2" value
  [[ "$(_env_existing "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_CAPTURED)" == true ]] \
    || return 1
  value="$(_env_existing "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_ENABLED)"
  _env_set "$lunarwing_env" TRANSCRIPTION_ENABLED "${value:-false}"
  value="$(_env_existing "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_PROVIDER)"
  _env_set "$lunarwing_env" TRANSCRIPTION_PROVIDER "${value:-openai}"
  value="$(_env_existing "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_MODEL)"
  _env_set "$lunarwing_env" TRANSCRIPTION_MODEL "${value:-whisper-1}"
  _env_set "$voice_env" VOICE_PREVIOUS_TRANSCRIPTION_CAPTURED false
}

configure_voice() {
  local name="$1" enabled="$2" voice_env lunarwing_env token voice_port stt_model
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  [[ "$enabled" == true || "$enabled" == false ]] || die "invalid LunarVoice state: $enabled"
  voice_env="$(tenant_env_dir "$name")/voice.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$voice_env" && ! -L "$voice_env" ]] || write_tenant_voice_env "$name" >/dev/null
  [[ -f "$lunarwing_env" && ! -L "$lunarwing_env" ]] \
    || die "tenant LunarWing environment missing or unsafe"
  if [[ "$enabled" == true ]]; then
    voice_models_ready "$name" || die "verified STT models missing under $VOICE_MODELS_DIR"
  fi
  token="$(_env_existing "$voice_env" VOICE_AUTH_TOKEN)"
  [[ -n "$token" ]] || die "LunarVoice bearer token is missing"
  stt_model="$(_env_existing "$voice_env" VOICE_STT_MODEL)"
  stt_model="${stt_model:-whisper-tiny}"
  voice_port="$(ports_get "$name" voice_service)"
  _voice_port_valid "$voice_port" || die "invalid voice_service port for tenant '$name'"
  _env_set "$voice_env" VOICE_ENABLED "$enabled"
  _env_set "$lunarwing_env" VOICE_SERVICE_URL "http://127.0.0.1:${voice_port}"
  _env_set "$lunarwing_env" VOICE_AUTH_TOKEN "$token"
  if [[ "$enabled" == true ]]; then
    _voice_capture_transcription_config "$voice_env" "$lunarwing_env"
    _env_set "$lunarwing_env" TRANSCRIPTION_PROVIDER lunarvoice
    _env_set "$lunarwing_env" TRANSCRIPTION_MODEL "$stt_model"
    _env_set "$lunarwing_env" TRANSCRIPTION_ENABLED true
  elif ! _voice_restore_transcription_config "$voice_env" "$lunarwing_env" \
      && [[ "$(_env_existing "$lunarwing_env" TRANSCRIPTION_PROVIDER)" == lunarvoice ]]; then
    _env_set "$lunarwing_env" TRANSCRIPTION_ENABLED false
  fi
  ports_set_voice "$name" "$enabled"
  say "LunarVoice enabled=$enabled for $name"
}

_voice_validate_uint() {
  local value="$1" minimum="$2" maximum="$3" number
  [[ "$value" =~ ^[0-9]+$ && ${#value} -le 10 ]] || return 1
  number=$((10#$value))
  (( number >= minimum && number <= maximum ))
}

_voice_validate_stt_language() {
  local value="${1,,}" codes
  codes="auto en zh de es ru ko fr ja pt tr pl ca nl ar sv it id hi fi vi he uk el ms cs ro da hu ta no th ur hr bg lt la mi ml cy sk te fa lv bn sr az sl kn et mk br eu is hy ne mn bs kk sq sw gl mr pa si km sn yo so af oc ka be tg sd gu am yi lo uz fo ht ps tk nn mt sa lb my bo tl mg as tt haw ln ha ba jw su"
  [[ " $codes " == *" $value "* ]]
}

_voice_validate_setting() {
  local key="$1" value="$2"
  case "$key" in
    VOICE_STT_ENGINE) [[ "$value" == whisper || "$value" == parakeet ]] ;;
    VOICE_STT_MODEL) [[ "$value" =~ ^[A-Za-z0-9._-]{1,128}$ ]] ;;
    VOICE_STT_LANGUAGE) _voice_validate_stt_language "$value" ;;
    VOICE_TTS_DEFAULT_VOICE) [[ "$value" =~ ^[A-Za-z0-9._-]{1,64}$ ]] ;;
    VOICE_ENABLE_VAD|VOICE_ENABLE_TTS) [[ "$value" == true || "$value" == false ]] ;;
    VOICE_MAX_AUDIO_SECS|VOICE_MAX_TTS_AUDIO_SECS|VOICE_REQUEST_TIMEOUT_SECS)
      _voice_validate_uint "$value" 1 3600
      ;;
    VOICE_MAX_AUDIO_BYTES) _voice_validate_uint "$value" 1024 1073741824 ;;
    VOICE_MAX_TEXT_CHARS) _voice_validate_uint "$value" 1 100000 ;;
    VOICE_MAX_CONCURRENT_INFERENCE) _voice_validate_uint "$value" 1 64 ;;
    VOICE_RATE_LIMIT_PER_SEC) _voice_validate_uint "$value" 1 10000 ;;
    VOICE_CACHE_TTL_SECS) _voice_validate_uint "$value" 0 86400 ;;
    VOICE_CACHE_MAX_ENTRIES) _voice_validate_uint "$value" 1 1000000 ;;
    ORT_NUM_THREADS) _voice_validate_uint "$value" 1 128 ;;
    *) return 1 ;;
  esac
}

configure_voice_settings() {
  local name="$1" voice_env lunarwing_env tmp assignment key value
  shift
  tenant_exists_in_registry "$name" \
    || { say "ERROR: tenant '$name' not found" >&2; return 1; }
  [[ $# -gt 0 ]] \
    || { say "ERROR: configure-voice requires at least one setting" >&2; return 1; }
  voice_env="$(tenant_env_dir "$name")/voice.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$voice_env" && ! -L "$voice_env" ]] \
    || { say "ERROR: LunarVoice is not installed for $name; run install-voice first" >&2; return 1; }
  [[ -f "$lunarwing_env" && ! -L "$lunarwing_env" ]] \
    || { say "ERROR: tenant LunarWing environment missing or unsafe" >&2; return 1; }

  tmp="$(mktemp "$(dirname "$voice_env")/.voice-config.XXXXXX")" || return 1
  cp --preserve=mode,ownership -- "$voice_env" "$tmp" \
    || { rm -f "$tmp"; say "ERROR: failed to stage LunarVoice configuration" >&2; return 1; }
  for assignment in "$@"; do
    if [[ "$assignment" != *=* ]]; then
      rm -f "$tmp"; say "ERROR: invalid voice setting" >&2; return 1
    fi
    key="${assignment%%=*}"
    value="${assignment#*=}"
    if ! _voice_validate_setting "$key" "$value"; then
      rm -f "$tmp"; say "ERROR: invalid value for $key" >&2; return 1
    fi
    _env_set "$tmp" "$key" "$value"
  done
  if tenant_voice_enabled "$name" && ! _voice_models_ready_env "$tmp"; then
    rm -f "$tmp"
    say "ERROR: configured STT model is not verified under $VOICE_MODELS_DIR" >&2
    return 1
  fi
  mv -T -- "$tmp" "$voice_env"
  chmod 0600 "$voice_env"
  chown "$name:$name" "$voice_env"
  if [[ "$(_env_existing "$lunarwing_env" TRANSCRIPTION_PROVIDER)" == lunarvoice ]]; then
    _env_set "$lunarwing_env" TRANSCRIPTION_MODEL \
      "$(_env_existing "$voice_env" VOICE_STT_MODEL)"
  fi
  say "LunarVoice configuration updated for $name"
}

install_voice_models() {
  local manifest="$1" installer="$SOURCE_REPO/projects/voice-sidecar/scripts/download-models.sh"
  [[ -f "$manifest" && -r "$manifest" && ! -L "$manifest" ]] \
    || die "model manifest must be a readable regular file"
  [[ -x "$installer" ]] || die "LunarVoice model installer missing: $installer"
  if [[ -e "$VOICE_MODELS_DIR" && ( ! -d "$VOICE_MODELS_DIR" || -L "$VOICE_MODELS_DIR" ) ]]; then
    die "unsafe LunarVoice model directory: $VOICE_MODELS_DIR"
  fi
  install -d -m 0755 -- "$VOICE_MODELS_DIR"
  "$installer" "$manifest" "$VOICE_MODELS_DIR"
}

install_tenant_voice() {
  local name="$1" manifest="${2:-}"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  ensure_container_runtime
  [[ -z "$manifest" ]] || install_voice_models "$manifest"
  if ! "$CONTAINER_RT" image inspect "$VOICE_SIDECAR_IMAGE" >/dev/null 2>&1; then
    build_voice_sidecar_image || die "failed to build LunarVoice sidecar image"
  fi
  _ensure_tenant_image "$name" "$VOICE_SIDECAR_IMAGE" \
    || die "failed to install LunarVoice image for tenant '$name'"
  write_tenant_voice_env "$name" >/dev/null
  say "LunarVoice installed for $name (inactive; run activate-voice when models are ready)"
}

_voice_daemon_running() {
  local name="$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service" >/dev/null 2>&1
  else
    rc-service "lunarwing-${name}" status >/dev/null 2>&1
  fi
}

_reload_voice_consumer_if_running() {
  local name="$1"
  if _voice_daemon_running "$name"; then
    _restart_tenant_daemon "$name" \
      || { say "ERROR: failed to restart LunarWing after voice configuration change" >&2; return 1; }
    say "LunarWing restarted; voice configuration is active for $name"
  else
    say "LunarWing is stopped; voice configuration applies on its next start"
  fi
}

apply_tenant_voice_configuration() {
  local name="$1"
  if tenant_voice_enabled "$name"; then
    start_tenant_voice "$name" || return 1
    _reload_voice_consumer_if_running "$name"
  else
    say "LunarVoice is inactive for $name; configuration applies on activation"
  fi
}

configure_tenant_voice() {
  local name="$1" voice_env lunarwing_env voice_backup lunarwing_backup result=0
  shift
  voice_env="$(tenant_env_dir "$name")/voice.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$voice_env" && ! -L "$voice_env" ]] \
    || die "LunarVoice is not installed for $name; run install-voice first"
  [[ -f "$lunarwing_env" && ! -L "$lunarwing_env" ]] \
    || die "tenant LunarWing environment missing or unsafe"
  voice_backup="$(mktemp "$(dirname "$voice_env")/.voice-rollback.XXXXXX")"
  lunarwing_backup="$(mktemp "$(dirname "$lunarwing_env")/.lunarwing-rollback.XXXXXX")"
  cp --preserve=mode,ownership -- "$voice_env" "$voice_backup" \
    && cp --preserve=mode,ownership -- "$lunarwing_env" "$lunarwing_backup" \
    || { rm -f -- "$voice_backup" "$lunarwing_backup"; return 1; }

  configure_voice_settings "$name" "$@" || {
    result=$?
    rm -f -- "$voice_backup" "$lunarwing_backup"
    return "$result"
  }
  apply_tenant_voice_configuration "$name" || result=$?
  if [[ "$result" -eq 0 ]]; then
    rm -f -- "$voice_backup" "$lunarwing_backup"
    return 0
  fi

  say "ERROR: LunarVoice configuration failed; restoring previous settings" >&2
  mv -T -- "$voice_backup" "$voice_env"
  mv -T -- "$lunarwing_backup" "$lunarwing_env"
  if tenant_voice_enabled "$name"; then
    start_tenant_voice "$name" \
      || say "WARNING: failed to restore the previous LunarVoice runtime" >&2
    _reload_voice_consumer_if_running "$name" \
      || say "WARNING: restart tenant '$name' to restore its prior transcription settings" >&2
  fi
  return "$result"
}

_rollback_voice_activation() {
  local name="$1" container="lunarwing-voice-$1"
  configure_voice "$name" false
  stop_tenant_voice "$name" || true
  remove_tenant_voice_unit "$name" || true
  if _ctr "$name" inspect "$container" >/dev/null 2>&1; then
    _ctr "$name" rm -f "$container" >/dev/null 2>&1 || true
  fi
  _reload_voice_consumer_if_running "$name" || true
}

activate_tenant_voice() {
  local name="$1" voice_env
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  voice_env="$(tenant_env_dir "$name")/voice.env"
  [[ -f "$voice_env" && ! -L "$voice_env" ]] \
    || die "LunarVoice is not installed for $name; run install-voice first"
  ensure_container_runtime
  _ensure_tenant_image "$name" "$VOICE_SIDECAR_IMAGE" \
    || die "LunarVoice image unavailable; run install-voice first"
  configure_voice "$name" true
  if ! start_tenant_voice "$name" || ! _reload_voice_consumer_if_running "$name"; then
    _rollback_voice_activation "$name"
    die "LunarVoice activation failed; previous transcription configuration restored"
  fi
}

remove_tenant_voice_unit() {
  local name="$1" container_name="lunarwing-voice-$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _remove_voice_systemd_fallback "$name" || return 1
    rm -f "$(tenant_quadlet_dir "$name")/${container_name}.container"
    rm -f "$(_hash_dir "$name")/${container_name}.hash"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
  else
    rc-service "${container_name}-sup" stop >/dev/null 2>&1 || true
    rc-update del "${container_name}-sup" default >/dev/null 2>&1 || true
    rm -f "$OPENRC_INITD_DIR/${container_name}-sup"
    rc-update del "$container_name" default >/dev/null 2>&1 || true
    rm -f "$OPENRC_INITD_DIR/$container_name"
    rm -f "$(_hash_dir "$name")/${container_name}.hash"
  fi
}

deactivate_tenant_voice() {
  local name="$1" voice_env
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  voice_env="$(tenant_env_dir "$name")/voice.env"
  [[ -f "$voice_env" && ! -L "$voice_env" ]] \
    || die "LunarVoice is not installed for $name; run install-voice first"
  configure_voice "$name" false
  stop_tenant_voice "$name" || return 1
  remove_tenant_voice_unit "$name" || return 1
  _reload_voice_consumer_if_running "$name"
}

_voice_remove_daemon_credentials() {
  local path="$1" tmp
  [[ -f "$path" && ! -L "$path" ]] || die "tenant LunarWing environment missing or unsafe"
  tmp="$(mktemp "$(dirname "$path")/.voice-remove.XXXXXX")"
  awk '!/^(VOICE_SERVICE_URL|VOICE_AUTH_TOKEN)=/' "$path" >"$tmp"
  chmod --reference="$path" "$tmp"
  chown --reference="$path" "$tmp"
  mv -T -- "$tmp" "$path"
}

remove_tenant_voice() {
  local name="$1" voice_env container="lunarwing-voice-$1"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  voice_env="$(tenant_env_dir "$name")/voice.env"
  configure_voice "$name" false
  stop_tenant_voice "$name" || return 1
  remove_tenant_voice_unit "$name" || return 1
  _voice_remove_daemon_credentials "$(tenant_env_dir "$name")/lunarwing.env"
  _reload_voice_consumer_if_running "$name" || return 1
  ensure_container_runtime
  if _ctr "$name" inspect "$container" >/dev/null 2>&1; then
    _ctr "$name" rm -f "$container" >/dev/null \
      || { say "ERROR: failed to remove LunarVoice container $container" >&2; return 1; }
  fi
  if [[ -e "$voice_env" || -L "$voice_env" ]]; then
    [[ -f "$voice_env" && ! -L "$voice_env" ]] \
      || die "refusing to remove unsafe LunarVoice environment path: $voice_env"
    rm -f -- "$voice_env"
  fi
  say "LunarVoice removed for $name (shared models and image retained)"
}
