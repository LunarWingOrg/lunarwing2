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

  # No source image in the admin store. A tenant that already holds one under
  # this name can still run: the admin store gates *distribution* (so a rebuilt
  # image reaches tenants), not whether an already-provisioned tenant may start.
  # Returning 1 here stranded LunarVoice after `upgrade-tenant` emptied the
  # admin store, because the caller aborts before reaching its reset-failed +
  # start and the unit stayed failed until an operator intervened (WI#229).
  if [[ -z "$admin_id" ]]; then
    [[ -n "$tenant_id" ]] && return 0
    return 1   # absent everywhere -> caller tells the operator to build first
  fi

  if [[ -n "$tenant_id" ]]; then
    say "refreshing stale image $image in ${name}'s rootless store (save|load — minutes for large images) ..."
  else
    say "distributing image $image into ${name}'s rootless store (save|load — minutes for large images) ..."
  fi
  # Capture each half's stderr separately so the failure class (missing
  # admin-side image, store corruption, EPIPE from a load side that died
  # early, ...) reaches the operator instead of collapsing into one generic
  # warning (WI#271).
  local save_err load_err
  save_err="$(mktemp)" || return 1
  load_err="$(mktemp)" || { rm -f "$save_err"; return 1; }
  if "$CONTAINER_RT" save "$image" 2>"$save_err" | _ctr "$name" load >/dev/null 2>"$load_err"; then
    rm -f "$save_err" "$load_err"
    # Drop the previous (now-untagged) image if the load re-pointed the tag, so
    # repeated worker-image updates don't accumulate GBs of stale layers in the
    # tenant's rootless store.
    _ctr "$name" image prune -f >/dev/null 2>&1 || true
    say "image $image available in ${name}'s store"
    return 0
  fi
  local save_msg load_msg detail=""
  save_msg="$(head -n 3 "$save_err" | tr '\n' ' ')"
  save_msg="${save_msg% }"
  load_msg="$(head -n 3 "$load_err" | tr '\n' ' ')"
  load_msg="${load_msg% }"
  rm -f "$save_err" "$load_err"
  [[ -n "$save_msg" ]] && detail="save: $save_msg"
  if [[ -n "$load_msg" ]]; then
    [[ -n "$detail" ]] && detail+="; "
    detail+="load: $load_msg"
  fi
  if [[ -n "$detail" ]]; then
    say "WARNING: failed to load $image into ${name}'s store ($detail)"
  else
    say "WARNING: failed to load $image into ${name}'s store"
  fi
  return 1
}

# Worker launch contract with the tenant-store image ID appended.
#
# _ensure_tenant_image refreshes a tenant's copy of a rebuilt worker image, but
# an existing container stays bound to the image ID it was created from: the
# OpenRC unit and the non-Quadlet fallback only `podman start` it, and an
# already-active Quadlet unit is not restarted by start-tenant. So after
# build-<worker>-worker the OLD image kept serving until an operator removed
# the container by hand (the kimi 2.2.0 rollout behind WI#411/WI#412 ran that
# way). Hashing the image ID into the container contract makes start-tenant
# force-recreate after every rebuild, as the vision/voice sidecar contracts
# (_vision_launch_contract / _voice_launch_contract) already do. The base
# contract is returned unchanged when the image cannot be inspected, so an
# inspect failure alone never forces a recreate.
# Usage: _worker_launch_contract <tenant> <image-ref> [<base-contract>]
_worker_launch_contract() {
  local name="$1" image="$2" base="${3:-}" image_id
  image_id="$(_ctr "$name" image inspect -f '{{.Id}}' "$image" 2>/dev/null)" || image_id=""
  if [[ -z "$image_id" ]]; then
    printf '%s' "$base"
    return 0
  fi
  if [[ -n "$base" ]]; then
    printf '%s;image=%s' "$base" "$image_id"
  else
    printf 'image=%s' "$image_id"
  fi
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
  _openrc_update add "${container}-sup" default >/dev/null 2>&1 || true
  _openrc_service "${container}-sup" start >/dev/null 2>&1 || true
}

# Deregister a babysitter unit (stop + boot-disable + remove).
_deregister_babysitter() {
  local container="$1"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/${container}-sup" ]] || return 0
  _openrc_service "${container}-sup" stop >/dev/null 2>&1 || true
  _openrc_update del "${container}-sup" default >/dev/null 2>&1 || true
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

# ── Generic podman sandbox image (WI#223) ────────────────────────────────────

SANDBOX_IMAGE_TAG=lunarwing-sandbox:latest
SANDBOX_DOCKERFILE="$REPO_ROOT/docker/sandbox.Dockerfile"

tenant_uses_managed_sandbox_image() {  # <tenant>
  local env_path
  env_path="$(tenant_env_dir "$1")/lunarwing.env"
  [[ "$(_env_existing "$env_path" SANDBOX_IMAGE)" == "$SANDBOX_IMAGE_TAG" ]]
}

# Match CliRunner::detect's probe order: Podman first for auto/podman, Docker
# first when a tenant explicitly set SANDBOX_RUNTIME=docker. mt-admin's worker
# runtime is independent and may be Docker on a host with both CLIs.
sandbox_container_runtime() {  # [tenant]
  local preferred=podman runtime
  if [[ -n "${1:-}" ]]; then
    preferred="$(_env_existing "$(tenant_env_dir "$1")/lunarwing.env" SANDBOX_RUNTIME)"
  fi
  local -a order=(podman docker)
  [[ "$preferred" == docker ]] && order=(docker podman)
  for runtime in "${order[@]}"; do
    if command -v "$runtime" >/dev/null 2>&1; then
      printf '%s' "$runtime"
      return 0
    fi
  done
  ensure_container_runtime
  printf '%s' "$CONTAINER_RT"
}

tenant_sandbox_rootless_prereqs_ready() {  # <tenant>
  local name="$1" uid
  uid="$(id -u "$name")" || return 1
  grep -q "^${name}:" /etc/subuid 2>/dev/null \
    && grep -q "^${name}:" /etc/subgid 2>/dev/null \
    && [[ -d "/run/user/$uid" ]] \
    && [[ "$(stat -c %u "/run/user/$uid" 2>/dev/null)" == "$uid" ]]
}

# Build the lean generic sandbox image into the admin (root) store. Rootless
# tenants get it distributed into their own store by
# ensure_tenant_sandbox_image. The Dockerfile has no COPY/ADD, so the build
# context is ic/docker/ rather than ic/: the work item's illustrative command
# used `ic/` as context, but ic/ carries the 1.3G vendor/ tree (which must
# stay in-context for Dockerfile.worker's offline cargo build and so cannot be
# dockerignored), and podman stages the whole context before starting.
build_sandbox_image() {
  [[ -f "$SANDBOX_DOCKERFILE" ]] \
    || { say "WARNING: $SANDBOX_DOCKERFILE missing; cannot build sandbox image"; return 1; }
  local runtime
  runtime="$(sandbox_container_runtime "${1:-}")" || return 1
  # On hosts whose Podman bridge cannot reach the host DNS resolver, apt in
  # the locally built sandbox image cannot resolve deb.debian.org. Match the
  # worker image builders: host networking is build-only, not sandbox runtime.
  local -a network_args=()
  [[ "$runtime" != podman ]] || network_args=(--network=host)
  "$runtime" build "${network_args[@]}" -t "$SANDBOX_IMAGE_TAG" \
    -f "$SANDBOX_DOCKERFILE" "$REPO_ROOT/docker" >/dev/null \
    || { say "WARNING: sandbox image build failed (run 'build-sandbox-image' to retry)"; return 1; }
  say "sandbox image built: $SANDBOX_IMAGE_TAG"
}

# Make the sandbox image available to the tenant that will run the daemon's
# sandboxed shell commands (rootless podman keeps a per-tenant store). Wraps
# _ensure_tenant_image so the same current-image distribution rules apply.
ensure_tenant_sandbox_image() {  # <tenant>
  local name="$1" runtime
  runtime="$(sandbox_container_runtime "$name")" || return 1
  ensure_container_runtime
  # A host that selected Docker for mt-admin workers may still have Podman on
  # the tenant PATH. Set up missing prerequisites before attempting save|load.
  # `system migrate` in ensure_rootless_prereqs stops Podman containers; never
  # run it on every start-tenant for an already-prepared user.
  if [[ "$runtime" == "podman" && "$MT_ROOTLESS" != "true" ]] \
      && ! tenant_sandbox_rootless_prereqs_ready "$name"; then
    if ! ( CONTAINER_RT=podman MT_ROOTLESS=true ensure_rootless_prereqs "$name" ); then
      say "WARNING: rootless Podman prerequisites failed for sandbox tenant '$name'" >&2
      return 1
    fi
  fi
  local CONTAINER_RT="$runtime" MT_ROOTLESS
  if [[ "$runtime" == "podman" ]]; then MT_ROOTLESS=true; else MT_ROOTLESS=false; fi
  _ensure_tenant_image "$name" "$SANDBOX_IMAGE_TAG"
}

write_tenant_vision_env() {
  local name="$1"
  local env_dir env_path
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/vision.env"
  # WI#390 (review IG-1): the preserved token comes from ONE snapshot read as
  # the tenant — root resolving the live tenant-writable path would follow a
  # cross-tenant symlink and this render would write the other tenant's
  # LUNARWING_AUTH_TOKEN into this tenant's vision.env.
  local vision_snapshot
  _snapshot_tenant_env "$name" "$env_path" vision_snapshot
  local token
  token="$(_env_existing "$vision_snapshot" LUNARWING_AUTH_TOKEN)"
  token="${token:-$(generate_token)}"
  # Resolve VL backend URL: explicit override wins, else fleet default.
  # Empty (LUNARWING_MT_VL_URL= and DEFAULT_VL_URL unset) = no VL line written;
  # sidecar comes up with VL disabled. Idempotent — vision.env is fully rewritten.
  local vl_url
  vl_url="${LUNARWING_MT_VL_URL:-$DEFAULT_VL_URL}"
  mkdir -p "$env_dir"
  # WI#390: write as the tenant through the atomic writer — root never opens
  # vision.env inside the tenant-writable env dir, so a pre-placed or raced
  # symlink there cannot redirect the write onto an attacker-chosen path. The
  # explicit returns keep the refusal fatal even when errexit is suppressed
  # (e.g. a caller's || fallback list).
  local rc=0
  if [[ -n "$vl_url" ]]; then
    cat <<ENVEOF | write_tenant_file_atomic "$name" "$env_path" 600 || rc=1
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
VL_URL=$vl_url
VL_MODEL=qwen3-vl
ENVEOF
  else
    cat <<ENVEOF | write_tenant_file_atomic "$name" "$env_path" 600 || rc=1
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
ENVEOF
  fi
  rm -f -- "$vision_snapshot"
  [[ "$rc" -eq 0 ]] || return 1
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
  # WI#390: create as the tenant through the atomic writer; a raced or
  # pre-placed symlink (including a dangling one, which -e alone misses) can no
  # longer make root truncate or create an attacker-chosen path.
  if [[ -e "$env_path" || -L "$env_path" ]]; then
    [[ -f "$env_path" && ! -L "$env_path" ]] \
      || die "unsafe LunarVoice environment file: $env_path"
  else
    write_tenant_file_atomic "$name" "$env_path" 600 </dev/null \
      || die "could not create LunarVoice environment file: $env_path"
  fi

  # WI#390 (review IG-1): the preserved token comes from ONE snapshot read as
  # the tenant, taken after the symlink refusal above — root resolving the
  # live tenant-writable path in the window before the _env_set commits would
  # follow a cross-tenant swap and write the other tenant's VOICE_AUTH_TOKEN
  # into this tenant's voice.env. The per-key presence checks below carry no
  # value across, and every commit re-validates the live path tenant-side.
  local voice_snapshot
  _snapshot_tenant_env "$name" "$env_path" voice_snapshot
  token="$(_env_existing "$voice_snapshot" VOICE_AUTH_TOKEN)"
  rm -f -- "$voice_snapshot"
  token="${token:-$(generate_token)}"
  enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_voice // false' "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$enabled" == true ]] || enabled=false

  # WI#390 (IY-3): every commit is fail-closed — a refused tenant-side write
  # must fail the writer (add_tenant's `|| die` otherwise never fires) instead
  # of being swallowed by the trailing printf.
  _env_set "$env_path" VOICE_ENABLED "$enabled" || return 1
  _voice_env_set_default "$env_path" VOICE_AUTH_TOKEN "$token" || return 1
  _env_set "$env_path" VOICE_ALLOW_UNAUTHENTICATED false || return 1
  _env_set "$env_path" VOICE_BIND_HOST 0.0.0.0 || return 1
  _env_set "$env_path" VOICE_PORT "$VOICE_SIDECAR_INTERNAL_PORT" || return 1
  _env_set "$env_path" VOICE_HEALTH_PORT "$VOICE_SIDECAR_HEALTH_PORT" || return 1
  _env_set "$env_path" VOICE_MODEL_DIR /models || return 1
  _env_set "$env_path" VOICE_AUTO_DOWNLOAD false || return 1
  _env_set "$env_path" VOICE_REQUIRE_STT true || return 1

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
    _voice_env_set_default "$env_path" "$key" "$value" || return 1
  done
  # WI#390: no trailing root chmod/chown — every _env_set commit above already
  # lands voice.env tenant-owned at mode 600 through the tenant-side atomic
  # writer, and root chmod/chown on the tenant path would follow a symlink
  # swapped into place.
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
    _openrc_service "lunarwing-${name}" status >/dev/null 2>&1
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
  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
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
  _openrc_service "$supervisor" stop >/dev/null 2>&1 && return 0
  _openrc_service "$supervisor" status 2>&1 | grep -Eqi 'stopped|inactive|dead'
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
      _openrc_service "$container" stop >/dev/null 2>&1 || true
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
      _openrc_service "$container" stop >/dev/null 2>&1 || true
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
  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  write_tenant_file_atomic "$name" "$env_path" 600 <"$backup" \
    || { rm -f -- "$backup"; return 1; }
  rm -f -- "$backup"
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
  # Keep rollback material outside the tenant-writable directory; root owns
  # this mode-0600 temp and cannot be raced through a tenant-side rename.
  backup="$(mktemp)"
  tenant_cat "$name" "$env_path" >"$backup" \
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

  backup="$(mktemp)"
  tenant_cat "$name" "$env_path" >"$backup" \
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
  _openrc_service "$supervisor" stop >/dev/null 2>&1 && return 0
  _openrc_service "$supervisor" status 2>&1 | grep -Eqi 'stopped|inactive|dead'
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
      _openrc_service "$container" stop >/dev/null 2>&1 || true
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
      _openrc_service "$container_name" stop >/dev/null 2>&1 || true
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

# Mirror the sidecar-side settings the daemon env also carries (WI#259): the
# gateway reads VOICE_MAX_TTS_AUDIO_SECS for its text_too_long pre-flight and
# VOICE_REQUEST_TIMEOUT_SECS for its staggered proxy timeout, so both keys must
# track voice.env on every mutation path, not only a full daemon-env re-render.
_sync_voice_runtime_env() {  # <name>
  local name="$1" voice_env lunarwing_env value
  voice_env="$(tenant_env_dir "$name")/voice.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$lunarwing_env" && ! -L "$lunarwing_env" ]] || return 0
  value="$(_env_existing "$voice_env" VOICE_MAX_TTS_AUDIO_SECS)"
  [[ -z "$value" ]] || _env_set "$lunarwing_env" VOICE_MAX_TTS_AUDIO_SECS "$value"
  value="$(_env_existing "$voice_env" VOICE_REQUEST_TIMEOUT_SECS)"
  [[ -z "$value" ]] || _env_set "$lunarwing_env" VOICE_REQUEST_TIMEOUT_SECS "$value"
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
  _sync_voice_runtime_env "$name"
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

configure_voice_settings() (
  local name="$1" voice_env lunarwing_env tmp stage_dir assignment key value
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

  stage_dir="$(mktemp -d)" || return 1
  trap 'rm -rf -- "$stage_dir"' EXIT
  tmp="$stage_dir/voice.env"
  if ! tenant_cat "$name" "$voice_env" >"$tmp"; then
    say "ERROR: failed to stage LunarVoice configuration" >&2
    return 1
  fi
  chmod 600 "$tmp"
  for assignment in "$@"; do
    if [[ "$assignment" != *=* ]]; then
      say "ERROR: invalid voice setting" >&2; return 1
    fi
    key="${assignment%%=*}"
    value="${assignment#*=}"
    if ! _voice_validate_setting "$key" "$value"; then
      say "ERROR: invalid value for $key" >&2; return 1
    fi
    _env_set "$tmp" "$key" "$value"
  done
  if tenant_voice_enabled "$name" && ! _voice_models_ready_env "$tmp"; then
    say "ERROR: configured STT model is not verified under $VOICE_MODELS_DIR" >&2
    return 1
  fi
  write_tenant_file_atomic "$name" "$voice_env" 600 <"$tmp" || return 1
  if [[ "$(_env_existing "$lunarwing_env" TRANSCRIPTION_PROVIDER)" == lunarvoice ]]; then
    _env_set "$lunarwing_env" TRANSCRIPTION_MODEL \
      "$(_env_existing "$voice_env" VOICE_STT_MODEL)"
  fi
  # Keep the daemon-side mirrors of the sidecar runtime bounds in step with
  # the just-updated voice.env (WI#259 review: configure-voice must not leave
  # the gateway enforcing a stale clip cap or request budget).
  _sync_voice_runtime_env "$name"
  say "LunarVoice configuration updated for $name"
)

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
    _openrc_service "lunarwing-${name}" status >/dev/null 2>&1
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
  voice_backup="$(mktemp)"
  lunarwing_backup="$(mktemp)"
  tenant_cat "$name" "$voice_env" >"$voice_backup" \
    && tenant_cat "$name" "$lunarwing_env" >"$lunarwing_backup" \
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
  write_tenant_file_atomic "$name" "$voice_env" 600 <"$voice_backup" \
    || { rm -f -- "$voice_backup" "$lunarwing_backup"; return 1; }
  write_tenant_file_atomic "$name" "$lunarwing_env" 600 <"$lunarwing_backup" \
    || { rm -f -- "$voice_backup" "$lunarwing_backup"; return 1; }
  rm -f -- "$voice_backup" "$lunarwing_backup"
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
    _openrc_service "${container_name}-sup" stop >/dev/null 2>&1 || true
    _openrc_update del "${container_name}-sup" default >/dev/null 2>&1 || true
    rm -f "$OPENRC_INITD_DIR/${container_name}-sup"
    _openrc_update del "$container_name" default >/dev/null 2>&1 || true
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
  local path="$1" name staged rc=1
  [[ -f "$path" && ! -L "$path" ]] || die "tenant LunarWing environment missing or unsafe"
  name="$(stat -c %U -- "$(dirname "$path")")" || return 1
  # Stage the filtered result first: a failed tenant-side read must leave the
  # environment untouched instead of publishing an empty replacement.
  staged="$(mktemp)" || return 1
  if tenant_cat "$name" "$path" \
      | awk '!/^(VOICE_SERVICE_URL|VOICE_AUTH_TOKEN)=/' >"$staged" \
    && write_tenant_file_atomic "$name" "$path" 600 <"$staged"; then
    rc=0
  fi
  rm -f -- "$staged"
  return "$rc"
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
