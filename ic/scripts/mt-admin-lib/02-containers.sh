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

write_tenant_voice_env() {
  local name="$1"
  local env_dir env_path token enabled stt_engine stt_model stt_language enable_vad enable_tts
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/voice.env"
  token="$(_env_existing "$env_path" VOICE_AUTH_TOKEN)"
  token="${token:-$(generate_token)}"
  enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_voice // false' "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$enabled" == true ]] || enabled=false
  stt_engine="$(_env_existing "$env_path" VOICE_STT_ENGINE)"; stt_engine="${stt_engine:-whisper}"
  stt_model="$(_env_existing "$env_path" VOICE_STT_MODEL)"; stt_model="${stt_model:-whisper-tiny}"
  stt_language="$(_env_existing "$env_path" VOICE_STT_LANGUAGE)"; stt_language="${stt_language:-auto}"
  enable_vad="$(_env_existing "$env_path" VOICE_ENABLE_VAD)"; enable_vad="${enable_vad:-true}"
  enable_tts="$(_env_existing "$env_path" VOICE_ENABLE_TTS)"; enable_tts="${enable_tts:-true}"
  mkdir -p "$env_dir"
  (
    umask 077
    cat >"$env_path" <<ENVEOF
VOICE_ENABLED=$enabled
VOICE_AUTH_TOKEN=$token
VOICE_BIND_HOST=0.0.0.0
VOICE_PORT=8090
VOICE_HEALTH_PORT=8091
VOICE_MODEL_DIR=/models
VOICE_STT_ENGINE=$stt_engine
VOICE_STT_MODEL=$stt_model
VOICE_STT_LANGUAGE=$stt_language
VOICE_ENABLE_VAD=$enable_vad
VOICE_ENABLE_TTS=$enable_tts
VOICE_REQUIRE_STT=true
VOICE_MAX_AUDIO_SECS=300
VOICE_MAX_AUDIO_BYTES=52428800
VOICE_MAX_TTS_AUDIO_SECS=300
VOICE_REQUEST_TIMEOUT_SECS=120
VOICE_MAX_CONCURRENT_INFERENCE=2
VOICE_RATE_LIMIT_PER_SEC=5
VOICE_CACHE_TTL_SECS=600
VOICE_CACHE_MAX_ENTRIES=5000
ORT_NUM_THREADS=4
ENVEOF
  )
  chown "$name:$name" "$env_path"
  printf '%s' "$token"
}

voice_models_ready() {
  local name="$1" env_path key value default_path host_path stt_model stt_engine marker_hash
  env_path="$(tenant_env_dir "$name")/voice.env"
  stt_model="$(_env_existing "$env_path" VOICE_STT_MODEL)"
  stt_model="${stt_model:-whisper-tiny}"
  stt_engine="$(_env_existing "$env_path" VOICE_STT_ENGINE)"
  stt_engine="${stt_engine:-whisper}"
  [[ "$stt_model" =~ ^[A-Za-z0-9._-]+$ ]] || return 1
  [[ -f "$VOICE_MODELS_DIR/.lunarvoice-${stt_model}.sha256" ]] || return 1
  marker_hash="$(tr -d '\r\n' <"$VOICE_MODELS_DIR/.lunarvoice-${stt_model}.sha256")"
  [[ "$marker_hash" =~ ^[0-9a-f]{64}$ ]] || return 1
  local model_paths
  if [[ "$stt_engine" == parakeet* ]]; then
    model_paths="VOICE_STT_ENCODER|/models/sherpa-onnx-nemo-${stt_model}/encoder.int8.onnx
VOICE_STT_DECODER|/models/sherpa-onnx-nemo-${stt_model}/decoder.int8.onnx
VOICE_STT_JOINER|/models/sherpa-onnx-nemo-${stt_model}/joiner.int8.onnx
VOICE_STT_TOKENS|/models/sherpa-onnx-nemo-${stt_model}/tokens.txt"
  else
    model_paths="VOICE_STT_ENCODER|/models/sherpa-onnx-whisper-tiny/tiny-encoder.int8.onnx
VOICE_STT_DECODER|/models/sherpa-onnx-whisper-tiny/tiny-decoder.int8.onnx
VOICE_STT_TOKENS|/models/sherpa-onnx-whisper-tiny/tiny-tokens.txt"
  fi
  while IFS='|' read -r key default_path; do
    value="$(_env_existing "$env_path" "$key")"
    value="${value:-$default_path}"
    [[ "$value" == /models/* ]] || return 1
    host_path="$VOICE_MODELS_DIR/${value#/models/}"
    [[ -f "$host_path" && -r "$host_path" ]] || return 1
  done <<<"$model_paths"
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
  local name="$1" vision_port health_port env_path container contract quadlet_file
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
  local name="$1" container="$2" running
  running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" || return 1
  [[ "$running" == "true" ]] || return 1
  _ctr "$name" exec "$container" curl -sf -o /dev/null --max-time 3 \
    "http://127.0.0.1:${VOICE_SIDECAR_HEALTH_PORT}/health" 2>/dev/null
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

configure_voice() {
  local name="$1" enabled="$2" voice_env lunarwing_env token voice_port stt_model
  tenant_exists_in_registry "$name" || die "tenant '$name' not found"
  voice_env="$(tenant_env_dir "$name")/voice.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$voice_env" ]] || write_tenant_voice_env "$name" >/dev/null
  [[ -f "$lunarwing_env" ]] || die "tenant LunarWing environment missing"
  if [[ "$enabled" == "true" ]]; then
    voice_models_ready "$name" || die "verified STT models missing under $VOICE_MODELS_DIR"
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
