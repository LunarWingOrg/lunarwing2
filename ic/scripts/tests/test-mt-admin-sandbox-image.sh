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
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
tenant_env_dir() { printf '%s/%s/env\n' "$fixture" "$1"; }
mkdir -p "$(tenant_env_dir tenant-a)"
runtime_selector="$(declare -f sandbox_container_runtime)"

# ── build_sandbox_image: podman path ─────────────────────────────────────────
# shellcheck disable=SC2034
CONTAINER_RT="podman"
CALLS=()
if build_sandbox_image; then
  ok "build_sandbox_image succeeds when the runtime build succeeds"
else
  bad "build_sandbox_image must succeed when the runtime build succeeds"
fi
expected="podman build --network=host -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker"
if [[ "${CALLS[0]:-}" == "$expected" ]]; then
  ok "podman build uses host networking and the sandbox Dockerfile with the ic/docker context"
else
  bad "unexpected podman invocation: [${CALLS[0]:-}] (expected [$expected])"
fi

# ── build_sandbox_image: Docker-only fallback ────────────────────────────────
sandbox_container_runtime() { printf docker; }
# shellcheck disable=SC2034
CONTAINER_RT="docker"
CALLS=()
build_sandbox_image >/dev/null
if [[ "${CALLS[0]:-}" == "docker build -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker" ]]; then
  ok "docker-only fallback builds in the Docker store"
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
eval "$runtime_selector"

# ── ensure_tenant_sandbox_image delegates to _ensure_tenant_image ────────────
_ensure_tenant_image() { CALLS+=("ensure-image:$1:$2:$CONTAINER_RT:$MT_ROOTLESS"); }
# shellcheck disable=SC2034  # read by ensure_tenant_sandbox_image
MT_ROOTLESS=true
CALLS=()
ensure_tenant_sandbox_image tenant-a
if [[ "${CALLS[0]:-}" == "ensure-image:tenant-a:lunarwing-sandbox:latest:podman:true" ]]; then
  ok "ensure_tenant_sandbox_image distributes the sandbox tag per tenant"
else
  bad "unexpected ensure delegation: [${CALLS[0]:-}]"
fi

# When mt-admin manages worker containers with Docker but Podman is installed,
# the sandbox still chooses Podman. Its image and prerequisites must land in
# the tenant's rootless Podman store rather than the shared Docker store.
ensure_rootless_prereqs() { printf 'prereqs:%s:%s\n' "$1" "$CONTAINER_RT" >>"$fixture/prereqs"; }
tenant_sandbox_rootless_prereqs_ready() { return 1; }
# shellcheck disable=SC2034  # read by ensure_tenant_sandbox_image
CONTAINER_RT=docker
# shellcheck disable=SC2034  # read by ensure_tenant_sandbox_image
MT_ROOTLESS=false
CALLS=()
ensure_tenant_sandbox_image tenant-a
if [[ "${CALLS[0]:-}" == "ensure-image:tenant-a:lunarwing-sandbox:latest:podman:true" ]] \
    && grep -Fqx 'prereqs:tenant-a:podman' "$fixture/prereqs"; then
  ok "dual-runtime host provisions the sandbox in the tenant's Podman store"
else
  bad "dual-runtime host selected the wrong store or missed rootless prerequisites"
fi
tenant_sandbox_rootless_prereqs_ready() { return 0; }
CALLS=()
ensure_tenant_sandbox_image tenant-a
if [[ "${CALLS[0]:-}" == "ensure-image:tenant-a:lunarwing-sandbox:latest:podman:true" ]] \
    && [[ "$(wc -l <"$fixture/prereqs")" -eq 1 ]]; then
  ok "preprovisioned rootless store skips disruptive system migrate on start"
else
  bad "preprovisioned rootless store was migrated again"
fi

# shellcheck disable=SC2034  # read by start_tenant and ensure_tenant_sandbox_image
CONTAINER_RT=podman
# shellcheck disable=SC2034  # read by ensure_tenant_sandbox_image
MT_ROOTLESS=true

# Only the managed image is provisioned; custom and legacy tenants keep their
# chosen runtime/image semantics on add-tenant and start-tenant.
printf 'SANDBOX_IMAGE=%s\n' "$SANDBOX_IMAGE_TAG" >"$(tenant_env_dir tenant-a)/lunarwing.env"
if tenant_uses_managed_sandbox_image tenant-a; then
  ok "managed tenant selects sandbox image distribution"
else
  bad "managed tenant did not select sandbox image distribution"
fi
printf 'SANDBOX_RUNTIME=docker\nSANDBOX_IMAGE=%s\n' "$SANDBOX_IMAGE_TAG" \
  >"$(tenant_env_dir tenant-a)/lunarwing.env"
CALLS=()
build_sandbox_image tenant-a
ensure_tenant_sandbox_image tenant-a
if [[ "${CALLS[0]:-}" == "docker build -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker" \
      && "${CALLS[1]:-}" == "ensure-image:tenant-a:lunarwing-sandbox:latest:docker:false" ]]; then
  ok "explicit Docker sandbox runtime builds and checks the Docker store"
else
  bad "explicit Docker sandbox runtime used a mismatched image store: ${CALLS[*]}"
fi
printf 'SANDBOX_IMAGE=custom-sandbox:v2\n' >"$(tenant_env_dir tenant-a)/lunarwing.env"
if tenant_uses_managed_sandbox_image tenant-a; then
  bad "custom tenant selected the managed sandbox image"
else
  ok "custom tenant keeps its image selection"
fi
printf 'LUNARWING_OWNER_ID=tenant-a\n' >"$(tenant_env_dir tenant-a)/lunarwing.env"
if tenant_uses_managed_sandbox_image tenant-a; then
  bad "legacy tenant without SANDBOX_IMAGE selected the managed image"
else
  ok "legacy tenant keeps its prior sandbox image on upgrade/start"
fi

# Exercise the actual start_tenant recovery path without touching systemd,
# OpenRC, a tenant account, or a container store. A failed distribution warns
# but does not prevent the base tenant from starting.
test_start_recovery() (
  local calls="$fixture/start-calls" distribution_fails=false failures=0
  mkdir -p "$fixture/tenant-a/run/ssh-agent"
  tenant_run_dir() { printf '%s/%s/run\n' "$fixture" "$1"; }
  tenant_exists_in_registry() { return 0; }
  tenant_darkirc_enabled() { return 1; }
  retire_tenant_nanocode() { :; }
  start_tenant_postgres() { printf 'postgres\n' >>"$calls"; }
  start_tenant_vision() { :; }
  start_tenant_voice() { :; }
  with_tenant_lifecycle_lock() { :; }
  warn_if_sshd_unreachable() { :; }
  upload_tenant_darkirc_secret() { :; }
  upload_tenant_brave_search_secret() { :; }
  patch_weechat_caps() { :; }
  tenant_worker_enabled() { return 1; }
  _ssh_ready_summary() { :; }
  _ensure_tenant_image() {
    printf 'ensure:%s:%s\n' "$1" "$2" >>"$calls"
    [[ "$distribution_fails" != true ]]
  }
  say() { printf '%s\n' "$*" >>"$calls"; }
  # shellcheck disable=SC2034  # read by start_tenant
  INIT_SYSTEM=systemd

  printf 'SANDBOX_IMAGE=%s\nSANDBOX_AUTO_PULL=false\n' "$SANDBOX_IMAGE_TAG" \
    >"$(tenant_env_dir tenant-a)/lunarwing.env"
  start_tenant tenant-a
  local ensure_line postgres_line
  ensure_line="$(grep -n '^ensure:' "$calls" | cut -d: -f1)"
  postgres_line="$(grep -n '^postgres$' "$calls" | cut -d: -f1)"
  if grep -Fqx 'ensure:tenant-a:lunarwing-sandbox:latest' "$calls" \
      && [[ "$ensure_line" -lt "$postgres_line" ]]; then
    ok 'start-tenant distributes the managed image before daemon startup'
  else
    bad 'start-tenant skipped pre-start sandbox image reconciliation'
  fi

  distribution_fails=true
  : >"$calls"
  start_tenant tenant-a
  if grep -Fq 'build-sandbox-image tenant-a' "$calls" && grep -Fqx 'postgres' "$calls"; then
    ok 'failed distribution gives a recovery command without stranding the tenant'
  else
    bad 'failed distribution lacked nonfatal recovery guidance'
  fi

  printf 'SANDBOX_IMAGE=custom-sandbox:v2\n' >"$(tenant_env_dir tenant-a)/lunarwing.env"
  : >"$calls"
  start_tenant tenant-a
  if ! grep -Fq 'ensure:' "$calls"; then
    ok 'start-tenant leaves a custom sandbox image alone'
  else
    bad 'start-tenant tried to distribute the managed image for a custom tenant'
  fi
  [[ "$failures" -eq 0 ]]
)
if ! test_start_recovery; then
  failures=$((failures + 1))
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
