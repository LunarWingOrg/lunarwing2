#!/usr/bin/env bash
# No images are built: exercise all mt-admin Pebble build entry points and pins.
# shellcheck disable=SC1091,SC2034 # sourced mt-admin and fixture globals
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
ensure_container_runtime() { :; }
say() { :; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

dockerfile="$LUNARWING_ROOT/pebble4lunarwing/Dockerfile"
default_digest="$(sed -n 's/^ARG PEBBLE_LOCK_SHA256=//p' "$dockerfile")"
actual_digest="$(sha256sum "$LUNARWING_ROOT/pebble4lunarwing/upstream/Cargo.lock" | cut -d' ' -f1)"
[[ "$default_digest" == "$actual_digest" ]] || fail 'reviewed default does not match committed lock'

capture_build() {
  [[ "$1" == build ]] || fail 'unexpected container command'
  shift
  effective_digest="$default_digest"
  effective_toolchains=false
  used_dockerfile=""
  while [[ $# -gt 1 ]]; do
    case "$1" in
      --build-arg)
        case "$2" in
          PEBBLE_LOCK_SHA256=*) effective_digest="${2#*=}" ;;
          WITH_TOOLCHAINS=*) effective_toolchains="${2#*=}" ;;
        esac
        shift 2 ;;
      -f) used_dockerfile="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  [[ "$used_dockerfile" == "$dockerfile" && "$1" == "$LUNARWING_ROOT" ]] \
    || fail 'wrong Dockerfile or context'
}
docker() { capture_build "$@"; }
podman() { capture_build "$@"; }

for CONTAINER_RT in docker podman; do
  for profile in false true; do
    for no_cache in false true; do
      build_pebble_worker "$no_cache" "$profile"
      [[ "$effective_digest" == "$actual_digest" ]] || fail "$CONTAINER_RT missing effective digest"
      [[ "$effective_toolchains" == "$profile" ]] || fail "$CONTAINER_RT wrong toolchain profile"
    done
  done
done
printf 'PASS: Docker/Podman Pebble builds use the reviewed lock pin in slim/fat profiles\n'
