#!/usr/bin/env bash
# LunarDepth lane configuration, image build, lifecycle, and smoke integration.

LUNARDEPTH_OVERLAY_BASE="${LUNARDEPTH_OVERLAY_BASE:-/var/lib/lunarwing/worker-overlays}"

_lunardepth_renderer() {
  printf '%s' "$LUNARWING_ROOT/lunardepth4lunarwing/scripts/render_overlay.py"
}

# Route validation imports the checkout's renderer module. mt-admin is root, so
# the interpreter runs isolated (-I) with a cleared environment: neither root's
# environment nor a module beside the working directory can reach it (ZP-6).
_lunardepth_validate_route() {  # <base-url> <model> <allow-insecure: true|false>
  local python
  python="$(command -v python3)" || die "python3 is required to validate the LunarDepth route"
  env -i PATH=/usr/bin:/bin PYTHONDONTWRITEBYTECODE=1 \
    "$python" -I - "$(_lunardepth_renderer)" "$1" "$2" "$3" <<'PY'
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
  _lunardepth_validate_route "$LUNARDEPTH_BASE_URL_RESOLVED" "$LUNARDEPTH_MODEL_RESOLVED" \
    "$LUNARDEPTH_ALLOW_INSECURE_RESOLVED" \
    || die "the LunarDepth route of tenant '$name' is not valid; run configure-lunardepth $name"
}

# Fleet creation (add-tenants). The LunarDepth key is visible to the model, so
# every tenant needs its own, independently revocable key (see
# docs/ops/WORKER-CONTAINERS.md). A single --lunardepth-api-key-file may
# therefore serve one tenant only; several tenants take a directory with one
# <tenant>.key file each (ZP-7).
lunardepth_fleet_key() {  # <tenant> <key dir>; prints the tenant's key
  local name="$1" key_dir="$2"
  [[ -d "$key_dir" && ! -L "$key_dir" ]] \
    || die "--lunardepth-api-key-dir is not a real directory: $key_dir"
  [[ ! -L "$key_dir/$name.key" ]] \
    || die "LunarDepth API key for tenant '$name' must not be a symlink: $key_dir/$name.key"
  _read_secret_file "$key_dir/$name.key" "LunarDepth API key for tenant '$name'"
}

lunardepth_fleet_keys_check() {  # <single key> <key dir> <placeholder: true|false> <tenant>...
  local single="$1" key_dir="$2" placeholder="$3"
  shift 3
  local name key digest other
  local -A seen=()
  [[ -z "$single" || -z "$key_dir" ]] \
    || die "use either --lunardepth-api-key-file or --lunardepth-api-key-dir, not both"
  if [[ -n "$key_dir" ]]; then
    [[ "$placeholder" != "true" ]] \
      || die "--lunardepth-api-key-dir and --lunardepth-allow-unauthenticated-gateway cannot be combined: no key would be stored"
    for name in "$@"; do
      key="$(lunardepth_fleet_key "$name" "$key_dir")" || exit 1
      # Compare digests: the key itself is neither an array index nor printed.
      digest="$(printf '%s' "$key" | sha256sum | cut -d' ' -f1)"
      other="${seen[$digest]:-}"
      [[ -z "$other" ]] \
        || die "tenants '$other' and '$name' have the same LunarDepth API key in $key_dir; each tenant needs its own"
      seen[$digest]="$name"
    done
    return 0
  fi
  if [[ -n "$single" && $# -gt 1 ]]; then
    die "--lunardepth-api-key-file would give $# tenants the same model-visible key; use --lunardepth-api-key-dir <dir> with one <tenant>.key file per tenant"
  fi
  if [[ -n "$single" && "$placeholder" == "true" ]]; then
    die "--lunardepth-api-key-file and --lunardepth-allow-unauthenticated-gateway cannot be combined: no key would be stored"
  fi
}

lunardepth_overlay_path() {
  printf '%s' "$LUNARDEPTH_OVERLAY_BASE/$1/lunarwing-lane.patch.yml"
}

LUNARDEPTH_OVERLAY_MAX_BYTES=65536

# Print the effective overlay on stdout, rendered by the tenant's own worker
# image from the template and the renderer baked into it.
#
# The task container renders the overlay again from that same template and
# refuses to start unless the bytes are equal. Rendering on the host from the
# operator checkout tied two revisions together: the first release that touched
# the template or the renderer broke every tenant whose image was older (ZP-5).
# One image now produces both sides.
#
# Image code runs only inside a throw-away container: no network, no
# capabilities, a read-only root filesystem, the image's unprivileged user. The
# host stores its bounded output as opaque bytes and never interprets it (ZP-6).
_lunardepth_render_in_image() {  # <tenant>; uses the values resolved by _lunardepth_route
  local name="$1"
  local -a args=(--template /app/config/lunarwing-lane.patch.yml --stdout
    --base-url "$LUNARDEPTH_BASE_URL_RESOLVED" --model "$LUNARDEPTH_MODEL_RESOLVED")
  [[ "$LUNARDEPTH_ALLOW_INSECURE_RESOLVED" != "true" ]] || args+=(--allow-insecure-http)
  # --no-healthcheck: the image defines one, and a one-shot container must not
  # ask the init system for a health timer (OpenRC hosts have none).
  _ctr "$name" run --rm --pull=never --network none --read-only --cap-drop ALL \
    --security-opt no-new-privileges --no-healthcheck --entrypoint /usr/bin/python3 \
    lunarwing-worker-lunardepth:latest -I /app/scripts/render_overlay.py "${args[@]}"
}

_lunardepth_overlay_size_ok() {  # <file>
  local size
  size="$(stat -c '%s' "$1" 2>/dev/null)" || return 1
  [[ "$size" =~ ^[0-9]+$ ]] && (( size > 0 && size <= LUNARDEPTH_OVERLAY_MAX_BYTES ))
}

# Called from start only, after the tenant image is known to exist. configure
# and the upgrade preflight validate the route and leave the installed overlay
# alone, so the file on disk always belongs to the units that are running.
render_lunardepth_overlay() {
  local name="$1" base tenant_dir output staged
  _lunardepth_route "$name"
  base="$LUNARDEPTH_OVERLAY_BASE"
  [[ ! -L "$base" ]] || die "LunarDepth overlay root must not be a symlink"
  install -d -o root -g root -m 0755 "$base"
  tenant_dir="$base/$name"
  [[ ! -L "$tenant_dir" ]] || die "LunarDepth overlay tenant path must not be a symlink"
  install -d -o root -g root -m 0555 "$tenant_dir"
  output="$(lunardepth_overlay_path "$name")"
  [[ ! -e "$output" && ! -L "$output" ]] || [[ -f "$output" && ! -L "$output" ]] \
    || die "LunarDepth overlay output must be a regular non-symlink file"
  staged="$(mktemp "$tenant_dir/.lunardepth-overlay.XXXXXX")" \
    || die "cannot stage the LunarDepth effective overlay"
  if ! _lunardepth_render_in_image "$name" >"$staged" || ! _lunardepth_overlay_size_ok "$staged"; then
    rm -f -- "$staged"
    die "the LunarDepth worker image could not render the effective overlay for '$name'; rebuild it with build-lunardepth-worker"
  fi
  if [[ -f "$output" ]] && cmp -s -- "$staged" "$output"; then
    rm -f -- "$staged"
  elif ! { chown root:root "$staged" && chmod 0444 "$staged" && mv -f -- "$staged" "$output"; }; then
    rm -f -- "$staged"
    die "could not install the LunarDepth effective overlay"
  fi
  [[ -f "$output" && ! -L "$output" && "$(stat -c '%u:%a' "$output")" == "0:444" ]] \
    || die "LunarDepth overlay ownership/mode verification failed"
  LUNARDEPTH_OVERLAY_FILE="$output"
}

_lunardepth_pair_running() {  # <tenant>
  local name="$1" container
  for container in "lunarwing-lunardepth-$name" "$(worker_task_container_name lunardepth "$name")"; do
    if _ctr_inspect "$name" -f '{{.State.Running}}' "$container" 2>/dev/null | grep -q true; then
      return 0
    fi
  done
  return 1
}

configure_lunardepth() {
  local name="$1" model="${2:-}" base_url="${3:-}" api_key="${4:-}"
  local allow_insecure="${5:-preserve}" allow_placeholder="${6:-preserve}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ports_migrate
  [[ -n "$model" || -n "$base_url" || -n "$api_key" || "$allow_insecure" != "preserve" || "$allow_placeholder" != "preserve" ]] \
    || die "configure-lunardepth: pass --model, --base-url, --api-key-file, or an explicit exception option"
  local env_dir existing_key supplied_key="$api_key"
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
  # A key handed over on this call must never be dropped without a word: with
  # the unauthenticated-gateway exception on, no key is written at all.
  if [[ -n "$supplied_key" && "$allow_placeholder" == "true" ]]; then
    die "configure-lunardepth: --api-key-file was given, but the unauthenticated-gateway exception is on and no key would be stored; add --require-api-key to use the key"
  fi
  LUNARDEPTH_BASE_URL_RESOLVED="$base_url" LUNARDEPTH_MODEL_RESOLVED="$model" \
    LUNARDEPTH_ALLOW_INSECURE_RESOLVED="$allow_insecure" LUNARDEPTH_ALLOW_PLACEHOLDER_RESOLVED="$allow_placeholder"
  _lunardepth_validate_route "$base_url" "$model" "$allow_insecure" \
    || die "configure-lunardepth: the route is not valid"

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
  ports_enable_worker "$name" lunardepth
  ensure_external_worker_config "$name" lunardepth lunardepth_wss \
    || die "could not safely write the LunarDepth external-worker config for tenant '$name'"
  # The validated id starts with its gateway dialect, tensormoon or tensorzero.
  if [[ "$allow_placeholder" == "true" ]]; then
    say "LunarDepth configured for tenant '$name' (explicit ${model%%::*}:: route; unauthenticated-gateway exception on, no key stored)"
  else
    say "LunarDepth configured for tenant '$name' (explicit ${model%%::*}:: route; key staged only in the task env)"
  fi
  # The effective overlay and its digest belong to the running units. They are
  # replaced together with those units, never underneath them: a half-applied
  # change would make the next automatic restart fail its digest check (ZP-5).
  if ( _lunardepth_pair_running "$name" ) >/dev/null 2>&1; then
    say "LunarDepth is running for '$name': applying the new configuration now (the worker pair restarts if it changed)"
    start_tenant_lunardepth "$name"
  else
    say "LunarDepth is not running for '$name'; the configuration applies at the next: $0 start-worker $name lunardepth"
  fi
}

# The pinned launcher reports "fully enforced" only from Landlock ABI 5, which
# Linux 6.10 introduced, and the lane refuses partial enforcement: the image
# build probes it and so does every task start. Checked here first, so a host
# below the floor is told why before a long build or a tenant stop. Only a
# definite negative counts; an unreadable LSM list is not a refusal.
LUNARDEPTH_LSM_FILE="${LUNARDEPTH_LSM_FILE:-/sys/kernel/security/lsm}"

lunardepth_landlock_floor_problem() {  # prints the reason and returns 0 when the host is below the floor
  local release lsm
  release="$(uname -r)"
  if [[ "$release" =~ ^([0-9]+)\.([0-9]+) ]]; then
    if (( 10#${BASH_REMATCH[1]} < 6 || (10#${BASH_REMATCH[1]} == 6 && 10#${BASH_REMATCH[2]} < 10) )); then
      printf 'kernel %s is older than Linux 6.10 (Landlock ABI 5)' "$release"
      return 0
    fi
  fi
  if [[ -r "$LUNARDEPTH_LSM_FILE" ]]; then
    lsm="$(<"$LUNARDEPTH_LSM_FILE")"
    if [[ ",$lsm," != *,landlock,* ]]; then
      printf 'landlock is not in the active LSM list (%s)' "$LUNARDEPTH_LSM_FILE"
      return 0
    fi
  fi
  return 1
}

_lunardepth_require_landlock_floor() {
  local problem
  if problem="$(lunardepth_landlock_floor_problem)"; then
    die "LunarDepth needs full Landlock enforcement on this host: $problem"
  fi
}

lunardepth_preflight() {
  local name="$1"
  # Validate only. The installed overlay must not change while the tenant still
  # runs the image it was rendered by.
  _lunardepth_route "$name"
  _lunardepth_require_landlock_floor
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
  _lunardepth_require_landlock_floor
  local source_date_epoch
  source_date_epoch="$(git -C "$LUNARWING_ROOT" show -s --format=%ct "$source_revision" 2>/dev/null || printf '0')"
  [[ "$source_date_epoch" =~ ^[0-9]+$ ]] || die "invalid LunarDepth source commit timestamp"
  local -a revision_args=(--build-arg "SOURCE_REVISION=$source_revision" --build-arg "SOURCE_DATE_EPOCH=$source_date_epoch")
  local -a cache_args=() toolchain_args=()
  [[ "$no_cache" != "true" ]] || cache_args=(--no-cache)
  [[ "$with_toolchains" != "true" ]] || toolchain_args=(--build-arg WITH_TOOLCHAINS=true)
  local candidate_tag="lunarwing-worker-lunardepth:candidate-$$" iid_file image_id output_tag
  output_tag="$(worker_image_ref lunardepth)"
  iid_file="$(mktemp "${TMPDIR:-/tmp}/lw-lunardepth-iid.XXXXXX")" || die "cannot create LunarDepth image-id file"
  local -a identity_args=(--iidfile "$iid_file")
  if [[ "$CONTAINER_RT" == "podman" ]]; then
    podman build "${cache_args[@]}" "${toolchain_args[@]}" "${revision_args[@]}" \
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

# Out-of-band verification of a candidate image. The image was built from
# network content, so nothing that reads it runs as root: the compile step and
# the verifier both run as the unprivileged nobody account, the verifier with an
# isolated interpreter and a cleared environment, on a private snapshot of the
# lane. This is the opencode2 pattern (WI#425).
_verify_lunardepth_image_out_of_band() (
  local image_ref="$1" source_root="$2" tmp cid archive config node_image _status
  local python nobody_uid nobody_gid inputs lane compiled
  tmp="$(mktemp -d /var/tmp/lw-lunardepth-oob.XXXXXX)" || return 1
  cid=''
  trap '_status=$?; [[ -z "$cid" ]] || "$CONTAINER_RT" rm -f "$cid" >/dev/null 2>&1 || true; rm -rf "$tmp"; exit "$_status"' EXIT
  python="$(command -v python3)" || return 1
  nobody_uid="$(id -u nobody 2>/dev/null)" || nobody_uid=''
  nobody_gid="$(id -g nobody 2>/dev/null)" || nobody_gid=''
  if [[ ! "$nobody_uid" =~ ^[1-9][0-9]*$ || ! "$nobody_gid" =~ ^[1-9][0-9]*$ ]]; then
    echo "ERROR: an unprivileged nobody account is required for out-of-band LunarDepth verification" >&2
    return 1
  fi
  cid="$("$CONTAINER_RT" create --pull=never "$image_ref" 2>/dev/null)" || cid=''
  [[ -n "$cid" ]] || return 1
  inputs="$tmp/inputs"
  lane="$tmp/lane"
  compiled="$tmp/scripts"
  install -d -m 0700 "$inputs" "$compiled" || return 1
  archive="$inputs/rootfs.tar"
  config="$inputs/image-config.json"
  "$CONTAINER_RT" export --output "$archive" "$cid" || return 1
  "$CONTAINER_RT" image inspect "$image_ref" >"$config" || return 1
  cp -R -- "$source_root/lunardepth4lunarwing" "$lane" || return 1
  rm -rf -- "$lane/scripts/node_modules"
  if [[ -n "$(find "$lane" ! -type f ! -type d -print -quit)" ]]; then
    echo "ERROR: the LunarDepth lane snapshot contains a link or special file" >&2
    return 1
  fi
  cp -- "$lane/scripts/package.json" "$lane/scripts/package-lock.json" \
    "$lane/scripts/tsconfig.json" "$lane/scripts/"*.ts "$compiled/" || return 1
  chown -R root:root "$lane" && chmod -R go-w,a+rX "$lane" || return 1
  chown -R "$nobody_uid:$nobody_gid" "$inputs" "$compiled" || return 1
  chmod 0711 "$tmp" && chmod 0700 "$inputs" "$compiled" && chmod 0600 "$archive" "$config" || return 1
  node_image='docker.io/library/node:22.23.4-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da'
  # The Node image is pinned by digest, so fetching it when it is missing is
  # safe. A BuildKit build keeps its base images in the build cache, not in the
  # image store, and --pull=never then found nothing on a Docker host.
  "$CONTAINER_RT" run --rm --pull=missing --network=host --cap-drop ALL \
    --security-opt no-new-privileges --user "$nobody_uid:$nobody_gid" \
    --env HOME=/tmp --env npm_config_cache=/tmp/npm-cache \
    --tmpfs /tmp:rw,exec,nosuid,size=512m --entrypoint /bin/bash \
    --volume "$compiled:/work:rw,z" "$node_image" \
    -c 'cd /work && npm ci --ignore-scripts && npx tsc -p tsconfig.json' || return 1
  if ! setpriv --reuid="$nobody_uid" --regid="$nobody_gid" --clear-groups --no-new-privs -- \
      env -i PATH=/usr/bin:/bin PYTHONDONTWRITEBYTECODE=1 \
      "$python" -I "$lane/scripts/verify_exposure_out_of_band.py" \
      --archive "$archive" --config "$config" --lane-root "$lane" --compiled-root "$compiled"; then
    echo "ERROR: out-of-band LunarDepth export verification failed" >&2
    return 1
  fi
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

_create_lunardepth_bridge() {  # <tenant> <launch contract> <wss port> [extra run arguments...]
  local name="$1" launch_contract="$2" wss_port="$3"
  shift 3
  # Everything after the port goes on the run command: the user-namespace
  # arguments and, on OpenRC, --no-healthcheck. Dropping the latter left the
  # image healthcheck on the bridge, and the next start removed the running
  # bridge again to get rid of it (ZP-3).
  local -a extra_run_args=("$@")
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
    "${extra_run_args[@]}" \
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
