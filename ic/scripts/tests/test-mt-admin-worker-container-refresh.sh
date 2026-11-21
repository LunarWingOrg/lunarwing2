#!/usr/bin/env bash
# WI#796: upgrade-tenant must rebuild external worker containers, not only
# retag their images. upgrade_refresh_worker_containers distributes each enabled
# lane's current image into the tenant store and removes the bridge/task
# containers that still run a different image, so the following start recreates
# them from the upgraded image. A lane already on the current image, a
# deselected lane and an intentionally stopped OpenRC lane are left alone; a
# failed distribution only warns and never removes a container.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

failures=0
fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# ── fixture ──────────────────────────────────────────────────────────────────
# STORE_IMAGES maps a lane to the image ID the tenant store holds; RUNNING_IMAGES
# maps a container to the image it was created from. ABSENT_IMAGE makes
# distribution fail for one lane.
declare -A STORE_IMAGES=()
declare -A RUNNING_IMAGES=()
ENABLED_LANES=""
STOPPED_LANES=""
ABSENT_IMAGE=""
CALLS=()
OUTPUT=""

say() { OUTPUT+="$*"$'\n'; }
warn_err() { OUTPUT+="WARN: $*"$'\n'; }
ensure_container_runtime() {
  # shellcheck disable=SC2034  # read by the sourced helper
  CONTAINER_RT="podman"
  # shellcheck disable=SC2034
  MT_ROOTLESS="true"
}
ensure_init_system() { :; }
podman_supports_quadlet() { [[ "${QUADLET:-true}" == true ]]; }
tenant_worker_enabled() { [[ " $ENABLED_LANES " == *" $2 "* ]]; }
tenant_worker_desired_running() { [[ " $STOPPED_LANES " != *" $2 "* ]]; }
_ensure_tenant_image() {  # <name> <image>
  CALLS+=("distribute:$2")
  [[ "$2" != "lunarwing-worker-${ABSENT_IMAGE}:latest" ]]
}
_systemctl_user() { CALLS+=("systemctl:$2:${3:-}"); return 0; }
_ctr() {
  local verb="$2"
  shift 2
  case "$verb $*" in
    "image inspect -f {{.Id}} "*)
      local ref="${!#}" lane
      lane="${ref#lunarwing-worker-}"
      lane="${lane%:latest}"
      [[ -n "${STORE_IMAGES[$lane]:-}" ]] || return 1
      printf '%s\n' "${STORE_IMAGES[$lane]}" ;;
    "rm -f "*)
      CALLS+=("rm:$*") ;;
    *)
      CALLS+=("ctr:$verb $*") ;;
  esac
  return 0
}
# _ctr_inspect is called as `_ctr_inspect <tenant> -f '{{.Image}}' <container>`
# and wraps the typed `container inspect` subcommand in production; the fixture
# answers the image query directly (the typed wrapper is pinned elsewhere).
_ctr_inspect() {
  local container="${!#}"
  [[ -n "${RUNNING_IMAGES[$container]:-}" ]] || return 1
  printf '%s\n' "${RUNNING_IMAGES[$container]}"
}

has_call() {
  local wanted="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "$wanted" ]] && return 0
  done
  return 1
}
assert_call() { if has_call "$1"; then pass "$2"; else fail "$2 (missing call: $1; got: ${CALLS[*]:-none})"; fi; }
assert_no_call() { if has_call "$1"; then fail "$2 (unexpected call: $1)"; else pass "$2"; fi; }

reset_fixture() {
  STORE_IMAGES=(
    [pebble]="sha256:newpebble"
    [opencode]="sha256:newopencode"
    [kimi]="sha256:newkimi"
    [opencode2]="sha256:newopencode2"
    [lunardepth]="sha256:newlunardepth"
  )
  RUNNING_IMAGES=()
  ENABLED_LANES="pebble opencode kimi opencode2 lunardepth"
  STOPPED_LANES=""
  ABSENT_IMAGE=""
  # shellcheck disable=SC2034  # read by the sourced helper
  INIT_SYSTEM=systemd
  CALLS=()
  OUTPUT=""
}

# ── a lane already on the upgraded image is untouched ────────────────────────
reset_fixture
RUNNING_IMAGES[lunarwing-pebble-alice]=sha256:newpebble
RUNNING_IMAGES[lunarwing-pebble-task-alice]=sha256:newpebble
upgrade_refresh_worker_containers alice
assert_no_call "rm:-f lunarwing-pebble-alice" 'current pebble bridge container is kept'
assert_no_call "rm:-f lunarwing-pebble-task-alice" 'current pebble task container is kept'

# ── a lane left on the previous image is rebuilt ─────────────────────────────
reset_fixture
RUNNING_IMAGES[lunarwing-pebble-alice]=sha256:oldpebble
RUNNING_IMAGES[lunarwing-pebble-task-alice]=sha256:oldpebble
RUNNING_IMAGES[lunarwing-kimi-alice]=sha256:newkimi
upgrade_refresh_worker_containers alice
assert_call "rm:-f lunarwing-pebble-alice" 'stale pebble bridge container is removed for rebuild (WI#796)'
assert_call "rm:-f lunarwing-pebble-task-alice" 'stale pebble task container is removed for rebuild (WI#796)'
assert_call "systemctl:stop:lunarwing-pebble-alice.service" 'stale pebble bridge unit is stopped before removal'
assert_call "systemctl:stop:lunarwing-pebble-task-alice.service" 'stale pebble task unit is stopped before removal'
assert_no_call "rm:-f lunarwing-kimi-alice" 'a current kimi container is not disturbed'
if [[ "$OUTPUT" == *"replacing lunarwing-pebble-alice"* ]]; then
  pass 'the replaced container is named in the upgrade log'
else
  fail "the replaced container is named in the upgrade log (got: $OUTPUT)"
fi

# ── OpenRC: an intentionally stopped lane keeps its containers ───────────────
reset_fixture
# shellcheck disable=SC2034  # read by the sourced helper
INIT_SYSTEM=openrc
STOPPED_LANES="pebble"
RUNNING_IMAGES[lunarwing-pebble-alice]=sha256:oldpebble
upgrade_refresh_worker_containers alice
assert_no_call "rm:-f lunarwing-pebble-alice" 'intentionally stopped OpenRC pebble container is left alone'
assert_no_call "distribute:lunarwing-worker-pebble:latest" 'intentionally stopped lane is not redistributed'

# ── a deselected lane is not touched ────────────────────────────────────────
reset_fixture
ENABLED_LANES="opencode"
RUNNING_IMAGES[lunarwing-pebble-alice]=sha256:oldpebble
upgrade_refresh_worker_containers alice
assert_no_call "rm:-f lunarwing-pebble-alice" 'deselected pebble container is left alone'
assert_no_call "distribute:lunarwing-worker-pebble:latest" 'deselected lane image is not distributed'

# ── a failed distribution warns and removes nothing ─────────────────────────
reset_fixture
ABSENT_IMAGE="pebble"
RUNNING_IMAGES[lunarwing-pebble-alice]=sha256:oldpebble
upgrade_refresh_worker_containers alice
assert_no_call "rm:-f lunarwing-pebble-alice" 'an undistributable image never removes the running container'
if [[ "$OUTPUT" == *"pebble image lunarwing-worker-pebble:latest is not available"* ]]; then
  pass 'the undistributable lane is named in the upgrade log'
else
  fail "the undistributable lane is named in the upgrade log (got: $OUTPUT)"
fi

# ── imperative/rootful path needs no unit stop ──────────────────────────────
reset_fixture
# shellcheck disable=SC2034  # read by the sourced helper
MT_ROOTLESS="false"
QUADLET=false
RUNNING_IMAGES[lunarwing-opencode-alice]=sha256:oldopencode
upgrade_refresh_worker_containers alice
assert_call "rm:-f lunarwing-opencode-alice" 'rootful stale opencode container is removed'
assert_no_call "systemctl:stop:lunarwing-opencode-alice.service" 'rootful path does not call the user manager'

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker container refresh tests passed\n'
