#!/usr/bin/env bash
# Standalone harness for the WI#223 generic sandbox image helpers
# (build_sandbox_image / ensure_tenant_sandbox_image). Sources
# lunarwing-mt-admin.sh (its BASH_SOURCE guard skips main() when sourced) and
# runs the helpers against a call-recording runtime mock.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
ok()   { printf 'PASS: %s\n' "$1"; }
bad()  { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

CALLS=()
# Consumed by the sourced mt-admin lib functions.
# shellcheck disable=SC2034
SANDBOX_IMAGE_TAG="lunarwing-sandbox:latest"
# NOTE: sourcing mt-admin redefines SCRIPT_DIR to ic/scripts, so derive the
# Dockerfile from the repo root the sourced script exports.
sandbox_dockerfile="$REPO_ROOT/docker/sandbox.Dockerfile"

# Runtime mock: record every invocation, succeed unless FAIL_BUILD is set.
podman() {
  CALLS+=("podman $*")
  [[ "${FAIL_BUILD:-}" != "true" ]]
}
docker() {
  CALLS+=("docker $*")
  [[ "${FAIL_BUILD:-}" != "true" ]]
}
ensure_container_runtime() { :; }
say() { :; }
FAIL_BUILD=""

# ── build_sandbox_image: podman path ─────────────────────────────────────────
# shellcheck disable=SC2034
CONTAINER_RT="podman"
CALLS=()
if build_sandbox_image; then
  ok "build_sandbox_image succeeds when the runtime build succeeds"
else
  bad "build_sandbox_image must succeed when the runtime build succeeds"
fi
expected="podman build --network=host --format docker -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker"
if [[ "${CALLS[0]:-}" == "$expected" ]]; then
  ok "podman build pinned to the sandbox Dockerfile with the ic/docker context"
else
  bad "unexpected podman invocation: [${CALLS[0]:-}] (expected [$expected])"
fi

# ── build_sandbox_image: docker path has no podman-only flags ────────────────
# shellcheck disable=SC2034
CONTAINER_RT="docker"
CALLS=()
build_sandbox_image >/dev/null
if [[ "${CALLS[0]:-}" == "docker build -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker" ]]; then
  ok "docker build omits --network=host/--format docker"
else
  bad "unexpected docker invocation: [${CALLS[0]:-}]"
fi

# ── build_sandbox_image: failure is reported, not fatal ──────────────────────
# shellcheck disable=SC2034
CONTAINER_RT="podman"
FAIL_BUILD="true"
CALLS=()
if build_sandbox_image; then
  bad "build_sandbox_image must report runtime build failure"
else
  ok "build_sandbox_image reports runtime build failure"
fi
FAIL_BUILD=""

# ── ensure_tenant_sandbox_image delegates to _ensure_tenant_image ────────────
_ensure_tenant_image() { CALLS+=("ensure-image:$1:$2"); }
CALLS=()
ensure_tenant_sandbox_image tenant-a
if [[ "${CALLS[0]:-}" == "ensure-image:tenant-a:lunarwing-sandbox:latest" ]]; then
  ok "ensure_tenant_sandbox_image distributes the sandbox tag per tenant"
else
  bad "unexpected ensure delegation: [${CALLS[0]:-}]"
fi

# ── The Dockerfile the build pins actually exists in the repo ────────────────
[[ -f "$sandbox_dockerfile" ]] \
  && ok "sandbox Dockerfile exists at ic/docker/sandbox.Dockerfile" \
  || bad "sandbox Dockerfile missing: $sandbox_dockerfile"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all WI#223 sandbox image helper tests passed\n'
