#!/usr/bin/env bash
# WI#511 / !2058 RU-2: reject builders without per-RUN network isolation before
# any image pruning, build, verification or promotion. No live runtime is used.
# shellcheck disable=SC2034  # fixture state is consumed by sourced helpers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
mkdir -p "$fixture/source/opencode2.4lunarwing/scripts"
printf 'fixture wrapper\n' >"$fixture/source/opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
export LUNARWING_ROOT="$fixture/source"
calls="$fixture/calls"
failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
say() { :; }
ensure_container_runtime() { :; }
_worker_source_revision() { printf 'fixture-revision\n'; }
_verify_opencode2_image_out_of_band() { printf 'verify:%s\n' "$1" >>"$calls"; }

formatted_version=4.5.0
fallback_version='podman version 4.5.0'
formatted_rc=0
fallback_rc=0
buildx_rc=0
built_image_id="sha256:$(printf '%064d' 0)"
podman() {
  case "${1:-}" in
    version) printf '%s\n' "$formatted_version"; return "$formatted_rc" ;;
    --version) printf '%s\n' "$fallback_version"; return "$fallback_rc" ;;
    *) fake_image_command podman "$@" ;;
  esac
}
docker() {
  if [[ "$*" == 'buildx version' ]]; then
    return "$buildx_rc"
  fi
  if [[ "${1:-}" == build ]]; then
    printf 'buildkit:%s\n' "${DOCKER_BUILDKIT-unset}" >>"$calls"
  fi
  fake_image_command docker "$@"
}
fake_image_command() {
  local runtime="$1" previous='' argument
  shift
  printf '%s:%s\n' "$runtime" "$*" >>"$calls"
  if [[ "${1:-}" == build ]]; then
    for argument in "$@"; do
      if [[ "$previous" == --iidfile ]]; then
        printf '%s\n' "$built_image_id" >"$argument"
      fi
      previous="$argument"
    done
  fi
}

assert_podman_version() {
  local version="$1" expected="$2" rc=0
  formatted_version="$version"
  CONTAINER_RT=podman
  podman_supports_run_network || rc=$?
  if [[ "$rc" -eq "$expected" ]]; then
    pass "Podman version '$version' returns $expected for inline RUN --network"
  else
    fail "Podman version '$version' returned $rc instead of $expected"
  fi
}
for version in 3.4.7 4.4.4 4.4.99 unknown 4.invalid; do
  assert_podman_version "$version" 1
done
for version in 4.5.0 4.5.1 4.6.0 5.8.2 6.1.4 10.0.0; do
  assert_podman_version "$version" 0
done
formatted_version=''
formatted_rc=1
fallback_version='podman version 4.5.0'
assert_podman_version '' 0
fallback_version='podman version 4.4.4'
assert_podman_version '' 1
fallback_version=''
fallback_rc=1
assert_podman_version '' 1
formatted_rc=0
fallback_rc=0

run_build_case() {  # <description> <expected status> [required diagnostic]
  local description="$1" expected="$2" diagnostic="${3:-}" rc
  : >"$calls"
  # Capture the CLI-like bare invocation: an if/|| test would suppress errexit.
  set +e
  (set -e; build_opencode2_worker true false "$fixture/source") >"$fixture/output" 2>&1
  rc=$?
  set -e
  if [[ "$rc" -ne "$expected" ]]; then
    fail "$description returned $rc instead of $expected"
    return
  fi
  if [[ "$expected" -ne 0 ]]; then
    if [[ ! -s "$calls" ]] && grep -Fq -- "$diagnostic" "$fixture/output"; then
      pass "$description refuses before any image side effects with an actionable diagnostic"
    else
      fail "$description touched images or missed its diagnostic"
    fi
  elif grep -Fq -- "verify:$built_image_id" "$calls" \
      && grep -Fq -- ":tag $built_image_id lunarwing-worker-opencode2:latest" "$calls"; then
    pass "$description builds, verifies and promotes the immutable image ID"
  else
    fail "$description did not complete guarded build and promotion"
  fi
}

CONTAINER_RT=podman
for formatted_version in 4.4.4 unknown; do
  run_build_case "unsupported Podman $formatted_version" 1 'Podman >= 4.5 (Buildah >= 1.30)'
done
formatted_version=''
fallback_version=''
run_build_case 'unreadable Podman version' 1 'RUN --network=none'
for formatted_version in 4.5.0 6.1.4; do
  run_build_case "supported Podman $formatted_version" 0
done

CONTAINER_RT=docker
DOCKER_BUILDKIT=0
run_build_case 'Docker legacy builder' 1 'enable DOCKER_BUILDKIT=1'
DOCKER_BUILDKIT=false
run_build_case 'Docker legacy builder selected with false' 1 'enable DOCKER_BUILDKIT=1'
DOCKER_BUILDKIT=invalid
run_build_case 'invalid Docker builder selection' 1 'legacy builder unsupported'
unset DOCKER_BUILDKIT
buildx_rc=1
run_build_case 'Docker without buildx' 1 'install buildx'
buildx_rc=0
run_build_case 'Docker with BuildKit selected implicitly' 0
if grep -Fxq 'buildkit:1' "$calls"; then
  pass 'Docker build explicitly enables BuildKit even when its environment is unset'
else
  fail 'Docker build did not force BuildKit'
fi
DOCKER_BUILDKIT=1
run_build_case 'Docker with BuildKit selected explicitly' 0
DOCKER_BUILDKIT=true
run_build_case 'Docker with BuildKit selected with true' 0

if [[ "$failures" -ne 0 ]]; then
  printf '\nRESULT: %d failures\n' "$failures"
  exit 1
fi
printf '\nRESULT: all opencode2 build-network tests passed\n'
