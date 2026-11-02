#!/usr/bin/env bash
# No images are built: exercise all mt-admin Pebble build entry points and pins.
# shellcheck disable=SC1091,SC2034 # sourced mt-admin and fixture globals
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"
ensure_container_runtime() { :; }
say() { :; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

dockerfile="$LUNARWING_ROOT/pebble4lunarwing/Dockerfile"
default_digest="$(sed -n 's/^ARG PEBBLE_LOCK_SHA256=//p' "$dockerfile")"
actual_digest="$(sha256sum "$LUNARWING_ROOT/pebble4lunarwing/upstream/Cargo.lock" | cut -d' ' -f1)"
[[ "$default_digest" == "$actual_digest" ]] || fail 'reviewed default does not match committed lock'

built_tag=""
tag_calls=()
rmi_calls=()
expected_root="$LUNARWING_ROOT"
capture_container_cmd() {
  case "$1" in
    build)
      shift
      effective_digest="$default_digest"
      effective_toolchains=false
      effective_revision=""
      used_dockerfile=""
      built_tag=""
      build_context=""
      while [[ $# -gt 1 ]]; do
        case "$1" in
          --build-arg)
            case "$2" in
              PEBBLE_LOCK_SHA256=*) effective_digest="${2#*=}" ;;
              WITH_TOOLCHAINS=*) effective_toolchains="${2#*=}" ;;
              SOURCE_REVISION=*) effective_revision="${2#*=}" ;;
            esac
            shift 2 ;;
          -f) used_dockerfile="$2"; shift 2 ;;
          -t) built_tag="$2"; shift 2 ;;
          *) shift ;;
        esac
      done
      [[ "$used_dockerfile" == "$expected_root/pebble4lunarwing/Dockerfile" && "$1" == "$expected_root" ]] \
        || fail "wrong Dockerfile or context for source root $expected_root"
      build_context="$1"
      ;;
    tag) tag_calls+=("$2 $3") ;;
    rmi) rmi_calls+=("$2") ;;
    *) fail "unexpected container command: $1" ;;
  esac
}
docker() { capture_container_cmd "$@"; }
podman() { capture_container_cmd "$@"; }

# WI#334: the fat build must smoke-check the task-shell toolchain anchors on
# the freshly built CANDIDATE image with the SAME runtime that built it, and
# only re-tag :latest after the probe passes; the slim build must do none of
# this (it has no toolchains).
smoke_calls=0
smoke_ref=""
smoke_rt=""
smoke_should_fail=false
mt_admin_smoke_stub() {
  smoke_calls=$((smoke_calls + 1))
  smoke_ref="${2:-}"
  smoke_rt="${CONTAINER_RT:-}"
  [[ "${1:-}" == image ]] || fail 'wrong smoke invocation'
  if [[ "$smoke_should_fail" == true ]]; then
    return 1
  fi
  return 0
}
export LUNARWING_PEBBLE_TOOLCHAIN_SMOKE=mt_admin_smoke_stub

for CONTAINER_RT in docker podman; do
  for profile in false true; do
    for no_cache in false true; do
      smoke_before=$smoke_calls
      tag_before=${#tag_calls[@]}
      rmi_before=${#rmi_calls[@]}
      build_pebble_worker "$no_cache" "$profile"
      [[ "$effective_digest" == "$actual_digest" ]] || fail "$CONTAINER_RT missing effective digest"
      [[ "$effective_toolchains" == "$profile" ]] || fail "$CONTAINER_RT wrong toolchain profile"
      if [[ "$profile" == true ]]; then
        [[ $smoke_calls -eq $((smoke_before + 1)) ]] \
          || fail "$CONTAINER_RT fat build skipped the toolchain smoke check"
        [[ "$built_tag" == lunarwing-worker-pebble:latest-candidate ]] \
          || fail "$CONTAINER_RT fat build did not target the candidate tag"
        [[ "$smoke_ref" == lunarwing-worker-pebble:latest-candidate ]] \
          || fail "$CONTAINER_RT smoke probed '$smoke_ref', not the candidate"
        [[ "$smoke_rt" == "$CONTAINER_RT" ]] \
          || fail "$CONTAINER_RT smoke ran with CONTAINER_RT='$smoke_rt'"
        [[ ${#tag_calls[@]} -eq $((tag_before + 1)) ]] \
          || fail "$CONTAINER_RT fat build did not re-tag :latest after the smoke"
        [[ "${tag_calls[-1]}" == "lunarwing-worker-pebble:latest-candidate lunarwing-worker-pebble:latest" ]] \
          || fail "$CONTAINER_RT unexpected tag: ${tag_calls[-1]}"
        [[ ${#rmi_calls[@]} -eq $((rmi_before + 1)) && "${rmi_calls[-1]}" == "lunarwing-worker-pebble:latest-candidate" ]] \
          || fail "$CONTAINER_RT fat build did not drop the candidate tag"
      else
        [[ $smoke_calls -eq $smoke_before ]] \
          || fail "$CONTAINER_RT slim build ran the toolchain smoke check"
        [[ "$built_tag" == lunarwing-worker-pebble:latest ]] \
          || fail "$CONTAINER_RT slim build did not target :latest directly"
        [[ ${#tag_calls[@]} -eq $tag_before && ${#rmi_calls[@]} -eq $rmi_before ]] \
          || fail "$CONTAINER_RT slim build unexpectedly re-tagged"
      fi
    done
  done
done

# A failed smoke must fail the build and must NOT re-tag :latest.
smoke_should_fail=true
tag_before=${#tag_calls[@]}
if ( build_pebble_worker false true ); then
  fail 'fat build survived a failing toolchain smoke check'
fi
[[ ${#tag_calls[@]} -eq $tag_before ]] \
  || fail 'failing smoke still re-tagged :latest'
smoke_should_fail=false

# WI#507: tenant builds use the tenant checkout, not the operator's dirty tree.
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
git_fixture() { git -C "$1" -c user.name=fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false "${@:2}"; }
make_pebble_checkout() {
  mkdir -p "$1/pebble4lunarwing/upstream"
  printf 'FROM scratch\n' >"$1/pebble4lunarwing/Dockerfile"
  cp "$LUNARWING_ROOT/pebble4lunarwing/upstream/Cargo.lock" "$1/pebble4lunarwing/upstream/Cargo.lock"
  git_fixture "$1" init -q
  git_fixture "$1" add -A
  git_fixture "$1" commit -qm fixture
}
operator_root="$fixture_dir/operator"
tenant_root="$fixture_dir/tenant"
make_pebble_checkout "$operator_root"
make_pebble_checkout "$tenant_root"
printf 'operator edit\n' >>"$operator_root/pebble4lunarwing/Dockerfile"
printf 'tenant commit\n' >>"$tenant_root/pebble4lunarwing/Dockerfile"
git_fixture "$tenant_root" commit -qam tenant
tenant_head="$(git -C "$tenant_root" rev-parse HEAD)"

expected_root="$tenant_root"
LUNARWING_ROOT="$operator_root" build_pebble_worker false false "$tenant_root"
[[ "$effective_revision" == "$tenant_head" ]] \
  || fail "tenant build stamp '$effective_revision' is not the tenant clone HEAD $tenant_head"
[[ "$build_context" == "$tenant_root" ]] \
  || fail "tenant build context '$build_context' is not the tenant clone"
expected_root="$LUNARWING_ROOT"

printf 'dirty tenant edit\n' >>"$tenant_root/pebble4lunarwing/Dockerfile"
if (build_pebble_worker false false "$tenant_root" >/dev/null 2>&1); then
  fail 'tenant build accepted a dirty pebble source tree'
fi

BUILD_LIB="$(dirname "$MT_ADMIN")/mt-admin-lib/05-tenant-build.sh"
grep -Fq 'build_pebble_worker "false" "$with_toolchains" "$(tenant_lw_root "$name")"' "$BUILD_LIB" \
  || fail 'build-tenant does not pass the tenant checkout to the pebble builder'
grep -Fq 'build_pebble_worker "false" "$with_toolchains" "$worker_source_root"' "$BUILD_LIB" \
  || fail 'build-all does not pass the verified shared worker root to the pebble builder'
grep -Fq 'pebble)    LUNARWING_WORKER_IMAGE_TAG="$tag" build_pebble_worker "false" "$with_toolchains" "$lw_root" || rc=$? ;;' "$BUILD_LIB" \
  || fail 'upgrade candidate builds do not pass the tenant checkout to the pebble builder'
grep -Fq 'worker_paths+=(pebble4lunarwing)' "$BUILD_LIB" \
  || fail 'build-all dirty-source guard does not cover pebble4lunarwing'
grep -Fq 'local smoke_script="${LUNARWING_PEBBLE_TOOLCHAIN_SMOKE:-$LUNARWING_ROOT/pebble4lunarwing/scripts/smoke-task-shell-toolchain.sh}"' "$BUILD_LIB" \
  || fail 'tenant fat builds can execute the smoke helper from the tenant checkout'

printf 'PASS: Pebble builds use the reviewed lock pin, clean tenant sources, and the operator smoke helper (WI#507)\n'
