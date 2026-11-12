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
real_ensure_tenant_image="$(declare -f _ensure_tenant_image)"
sandbox_digest="$(sandbox_image_input_digest "$REPO_ROOT")"

# ── build_sandbox_image: podman path ─────────────────────────────────────────
# shellcheck disable=SC2034
CONTAINER_RT="podman"
CALLS=()
if build_sandbox_image; then
  ok "build_sandbox_image succeeds when the runtime build succeeds"
else
  bad "build_sandbox_image must succeed when the runtime build succeeds"
fi
expected="podman build --network=host --build-arg DEBIAN_SNAPSHOT=20261105T082236Z --label org.lunarwing.sandbox.inputs=$sandbox_digest -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker"
if [[ "${CALLS[0]:-}" == "$expected" ]]; then
  ok "podman build uses host networking and labels the digest of its Dockerfile and snapshot"
else
  bad "unexpected podman invocation: [${CALLS[0]:-}] (expected [$expected])"
fi

# ── build_sandbox_image: Docker-only fallback ────────────────────────────────
sandbox_container_runtime() { printf docker; }
# shellcheck disable=SC2034
CONTAINER_RT="docker"
CALLS=()
build_sandbox_image >/dev/null
if [[ "${CALLS[0]:-}" == "docker build --build-arg DEBIAN_SNAPSHOT=20261105T082236Z --label org.lunarwing.sandbox.inputs=$sandbox_digest -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker" ]]; then
  ok "docker-only fallback builds in the Docker store with the same input label"
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
if [[ "${CALLS[0]:-}" == "docker build --build-arg DEBIAN_SNAPSHOT=20261105T082236Z --label org.lunarwing.sandbox.inputs=$sandbox_digest -t lunarwing-sandbox:latest -f $SANDBOX_DOCKERFILE $REPO_ROOT/docker" \
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

# ── WI#596: digest-gated candidates, promotion, failure, and rollback ───────
TENANT_LIFECYCLE_LOCK_DIR="$fixture/locks"
mkdir -p "$TENANT_LIFECYCLE_LOCK_DIR"
_ensure_marker_dir() { mkdir -p "$TENANT_LIFECYCLE_LOCK_DIR"; }
operator_repo="$fixture/operator"
tenant_repo="$fixture/tenant"
candidate_source="$operator_repo/ic"
mkdir -p "$candidate_source/docker"
cp "$SANDBOX_DOCKERFILE" "$candidate_source/docker/sandbox.Dockerfile"
git -C "$fixture" init -q operator
git -C "$operator_repo" add ic/docker/sandbox.Dockerfile
git -C "$operator_repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'Managed sandbox input'
git clone -q "$operator_repo" "$tenant_repo"
# shellcheck disable=SC2034  # read by the sourced operator-archive helpers
LUNARWING_ROOT="$operator_repo"
# shellcheck disable=SC2034  # read by the sourced operator-archive helpers
LUNARWING_WORKER_BUILD_TMPDIR="$fixture/archives"
mkdir -p "$LUNARWING_WORKER_BUILD_TMPDIR"
tenant_lw_root() { printf '%s\n' "$tenant_repo"; }
candidate_ref="$(upgrade_candidate_sandbox_image_ref tenant-a)"
candidate_marker="$(tenant_candidate_sandbox_file tenant-a)"
warnings="$fixture/warnings"
say() { printf '%s\n' "$*" >>"$warnings"; }

# A small stateful runtime double: image IDs survive tag removal so rollback
# exercises the same re-tag-by-ID contract as the real container stores.
declare -A IMAGE_IDS=() IMAGE_INPUTS=() IMAGE_ID_EXISTS=()
build_seq=0
register_image() {
  IMAGE_IDS["$1"]="$2"
  IMAGE_INPUTS["$2"]="${3:-outdated}"
  IMAGE_ID_EXISTS["$2"]=1
}
sandbox_runtime_mock() {
  local runtime="$1"
  shift
  CALLS+=("$runtime $*")
  case "${1:-}" in
    build)
      [[ "${FAIL_BUILD:-}" != true ]] || return 1
      local ref="" id dockerfile="" build_root
      while [[ $# -gt 0 ]]; do
        case "$1" in
          -t) ref="$2"; shift 2 ;;
          -f) dockerfile="$2"; shift 2 ;;
          *) shift ;;
        esac
      done
      [[ -n "$dockerfile" ]] || return 1
      printf '%s\n' "$dockerfile" >"$fixture/last-build-dockerfile"
      build_root="${dockerfile%/docker/sandbox.Dockerfile}"
      [[ "$build_root" != "$dockerfile" ]] || return 1
      build_seq=$((build_seq + 1))
      id="sandbox-new-$build_seq"
      register_image "$ref" "$id" "$(sandbox_image_input_digest "$build_root")"
      ;;
    image)
      [[ "${2:-}" == inspect ]] || return 0
      local ref="${!#}" id
      id="${IMAGE_IDS[$ref]:-}"
      [[ -n "$id" ]] || return 1
      if [[ "$*" == *'{{.Id}}'* ]]; then
        printf '%s\n' "$id"
      else
        printf '%s\n' "${IMAGE_INPUTS[$id]:-}"
      fi
      ;;
    tag)
      local src="$2" dst="$3" id
      if [[ -n "${IMAGE_IDS[$src]:-}" ]]; then
        id="${IMAGE_IDS[$src]}"
      elif [[ -n "${IMAGE_ID_EXISTS[$src]:-}" ]]; then
        id="$src"
      else
        return 1
      fi
      IMAGE_IDS["$dst"]="$id"
      ;;
    rmi)
      unset 'IMAGE_IDS[$2]'
      ;;
  esac
  return 0
}
podman() { sandbox_runtime_mock podman "$@"; }
docker() { sandbox_runtime_mock docker "$@"; }
_ctr() { shift; "$CONTAINER_RT" "$@"; }

printf 'SANDBOX_IMAGE=%s\n' "$SANDBOX_IMAGE_TAG" >"$(tenant_env_dir tenant-a)/lunarwing.env"
current_digest="$(sandbox_image_input_digest "$candidate_source")"
register_image "$SANDBOX_IMAGE_TAG" sandbox-old "$current_digest"
CALLS=()
build_candidate_sandbox_image tenant-a auto
if [[ "${CALLS[*]}" != *' build '* && ! -s "$candidate_marker" ]]; then
  ok 'unchanged input digest skips the candidate build and promotion marker'
else
  bad "unchanged inputs triggered sandbox work: ${CALLS[*]}"
fi

# The add-tenant helper uses the same digest gate and does no work unchanged.
CALLS=()
ensure_sandbox_image_current_from_tenant tenant-a auto
if [[ "${CALLS[*]}" != *' build '* ]]; then
  ok 'add-tenant input gate leaves a current admin image untouched'
else
  bad "add-tenant rebuilt an unchanged image: ${CALLS[*]}"
fi

# Distribution is also a no-op when admin and tenant IDs already match.
eval "$real_ensure_tenant_image"
CONTAINER_RT=podman
MT_ROOTLESS=true
tenant_sandbox_rootless_prereqs_ready() { return 0; }
CALLS=()
ensure_tenant_sandbox_image tenant-a
if [[ "${CALLS[*]}" != *' save '* && "${CALLS[*]}" != *' load '* ]]; then
  ok 'unchanged image IDs cause no rootless save/load redistribution'
else
  bad "unchanged image IDs were redistributed: ${CALLS[*]}"
fi

register_image "$SANDBOX_IMAGE_TAG" sandbox-old outdated
CALLS=()
rm -f "$fixture/last-build-dockerfile"
build_candidate_sandbox_image tenant-a auto
new_id="${IMAGE_IDS[$candidate_ref]:-}"
if [[ -n "$new_id" && "${IMAGE_IDS[$SANDBOX_IMAGE_TAG]}" == sandbox-old \
    && "${IMAGE_INPUTS[$new_id]:-}" == "$current_digest" \
    && "$(cat "$candidate_marker")" == podman ]]; then
  ok 'changed inputs build a candidate with the committed digest while latest stays old'
else
  bad "changed candidate did not preserve latest before promotion: ${CALLS[*]}"
fi
build_dockerfile="$(cat "$fixture/last-build-dockerfile" 2>/dev/null || true)"
if [[ "$build_dockerfile" == "$fixture/archives/"* \
    && "$build_dockerfile" != "$tenant_repo/"* ]]; then
  ok 'root builds only a private operator-archive Dockerfile, never the tenant tree'
else
  bad "sandbox build used an untrusted Dockerfile: $build_dockerfile"
fi
promote_candidate_sandbox_image tenant-a
if [[ "${IMAGE_IDS[$SANDBOX_IMAGE_TAG]:-}" == "$new_id" \
    && "$(awk -F '\t' '$1 == "sandbox" {print $2}' "$(_promote_rollback_file tenant-a)")" == sandbox-old ]]; then
  ok 'promotion moves latest to the candidate and records the previous ID'
else
  bad 'sandbox promotion or rollback record is incorrect'
fi
restore_promoted_workers tenant-a
if [[ "${IMAGE_IDS[$SANDBOX_IMAGE_TAG]:-}" == sandbox-old ]]; then
  ok 'upgrade rollback restores the previous sandbox image ID'
else
  bad 'sandbox rollback did not restore the previous image ID'
fi

# A forced candidate failure is non-fatal to the overall candidate train and
# leaves both the old tag and its tenant-upgrade recovery path intact.
: >"$warnings"
FAIL_BUILD=true
CALLS=()
if build_candidate_workers tenant-a false false false false false '' false auto \
    && [[ "${IMAGE_IDS[$SANDBOX_IMAGE_TAG]:-}" == sandbox-old ]] \
    && [[ ! -s "$candidate_marker" ]] \
    && grep -Fq 'keeping the previous lunarwing-sandbox:latest' "$warnings" \
    && grep -Fq 'build-sandbox-image tenant-a' "$warnings"; then
  ok 'failed candidate keeps the old image and continues with recovery guidance'
else
  bad "failed candidate changed latest or aborted the upgrade: ${CALLS[*]}"
fi
FAIL_BUILD=""

# add-tenant on an existing host also refreshes a stale tagged image.
CALLS=()
ensure_sandbox_image_current_from_tenant tenant-a auto
if [[ "${IMAGE_IDS[$SANDBOX_IMAGE_TAG]:-}" != sandbox-old \
    && "${IMAGE_INPUTS[${IMAGE_IDS[$SANDBOX_IMAGE_TAG]}]:-}" == "$current_digest" ]]; then
  ok 'add-tenant rebuilds an existing tag when the managed inputs changed'
else
  bad "add-tenant retained a stale image: ${CALLS[*]}"
fi

# A tenant-controlled working-tree edit must never become root's build input.
# Both tenant-driven paths reject the dirty source before any runtime build and
# preserve the current shared image (WI#596 review AAA-1).
for source_mode in upgrade add; do
  before_id="${IMAGE_IDS[$SANDBOX_IMAGE_TAG]:-}"
  rm -f "$fixture/last-build-dockerfile"
  printf '# tenant-writable mutation must not be built\n' \
    >>"$tenant_repo/ic/docker/sandbox.Dockerfile"
  if [[ "$source_mode" == upgrade ]]; then
    ( build_candidate_sandbox_image tenant-a always ) >"$fixture/tamper.out" 2>&1 && tamper_rc=0 || tamper_rc=$?
  else
    ( ensure_sandbox_image_current_from_tenant tenant-a always ) >"$fixture/tamper.out" 2>&1 && tamper_rc=0 || tamper_rc=$?
  fi
  if [[ "$tamper_rc" -ne 0 && ! -e "$fixture/last-build-dockerfile" \
      && "${IMAGE_IDS[$SANDBOX_IMAGE_TAG]:-}" == "$before_id" ]]; then
    ok "$source_mode refuses a tenant-modified Dockerfile without changing the image"
  else
    bad "$source_mode built or promoted tenant-writable Dockerfile bytes"
  fi
  git -C "$tenant_repo" checkout -- ic/docker/sandbox.Dockerfile
done

for image_mode in custom legacy; do
  if [[ "$image_mode" == custom ]]; then
    printf 'SANDBOX_IMAGE=custom-sandbox:v2\n' >"$(tenant_env_dir tenant-a)/lunarwing.env"
  else
    printf 'LUNARWING_OWNER_ID=tenant-a\n' >"$(tenant_env_dir tenant-a)/lunarwing.env"
  fi
  CALLS=()
  build_candidate_sandbox_image tenant-a always
  if [[ "${CALLS[*]}" != *' build '* ]]; then
    ok "$image_mode tenant is untouched even with sandbox policy always"
  else
    bad "$image_mode tenant triggered a managed build: ${CALLS[*]}"
  fi
done

printf 'SANDBOX_IMAGE=%s\n' "$SANDBOX_IMAGE_TAG" >"$(tenant_env_dir tenant-a)/lunarwing.env"
CALLS=()
build_candidate_sandbox_image tenant-a never
if [[ "${CALLS[*]}" != *' build '* ]]; then
  ok 'sandbox policy never suppresses the upgrade build'
else
  bad "sandbox policy never built an image: ${CALLS[*]}"
fi

# Docker uses the same candidate contract without Podman's host-network flag.
printf 'SANDBOX_RUNTIME=docker\nSANDBOX_IMAGE=%s\n' "$SANDBOX_IMAGE_TAG" \
  >"$(tenant_env_dir tenant-a)/lunarwing.env"
register_image "$SANDBOX_IMAGE_TAG" docker-old outdated
CALLS=()
build_candidate_sandbox_image tenant-a auto
docker_candidate_id="${IMAGE_IDS[$candidate_ref]:-}"
promote_candidate_sandbox_image tenant-a
if [[ -n "$docker_candidate_id" \
    && "${IMAGE_IDS[$SANDBOX_IMAGE_TAG]:-}" == "$docker_candidate_id" \
    && "${CALLS[*]}" == *'docker build --build-arg'* \
    && "${CALLS[*]}" != *'docker build --network=host'* ]]; then
  ok 'Docker sandbox runtime builds and promotes without host networking'
else
  bad "Docker candidate path used the wrong store or network: ${CALLS[*]}"
fi

# shellcheck disable=SC2034  # read by sandbox_debian_snapshot in sourced mt-admin
LUNARWING_MT_SANDBOX_DEBIAN_SNAPSHOT=20260101T000000Z
if [[ "$(sandbox_image_input_digest "$candidate_source")" != "$current_digest" ]]; then
  ok 'Debian snapshot override changes the managed input digest'
else
  bad 'Debian snapshot override was omitted from the input digest'
fi
unset LUNARWING_MT_SANDBOX_DEBIAN_SNAPSHOT

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
  local init ensure_line postgres_line
  printf 'SANDBOX_IMAGE=%s\nSANDBOX_AUTO_PULL=false\n' "$SANDBOX_IMAGE_TAG" \
    >"$(tenant_env_dir tenant-a)/lunarwing.env"
  for init in systemd openrc; do
    # shellcheck disable=SC2034  # read by start_tenant
    INIT_SYSTEM="$init"
    : >"$calls"
    start_tenant tenant-a
    ensure_line="$(grep -n '^ensure:' "$calls" | cut -d: -f1)"
    postgres_line="$(grep -n '^postgres$' "$calls" | cut -d: -f1)"
    if grep -Fqx 'ensure:tenant-a:lunarwing-sandbox:latest' "$calls" \
        && [[ "$ensure_line" -lt "$postgres_line" ]]; then
      ok "$init start-tenant distributes the managed image before daemon startup"
    else
      bad "$init start-tenant skipped pre-start sandbox image reconciliation"
    fi
  done

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

# WI#537: the managed image carries a reproducible full Python development
# suite under the same fixed-client HTTPS-snapshot apt contract as the workers.
python_packages=(
  'python3=3.11.2-1+b1'
  'python3-dev=3.11.2-1+b1'
  'python3-pip=23.0.1+dfsg-1'
  'python3-venv=3.11.2-1+b1'
  'python3-setuptools=66.1.1-1+deb12u2'
  'python3-wheel=0.38.4-2'
  'python3-build=0.9.0-1'
  'python3-pytest=7.2.1-2'
  'python3-pytest-cov=4.0.0-1'
  'python3-coverage=6.5.0+dfsg1-2+b1'
  'python3-mypy=1.0.1-1'
  'mypy=1.0.1-1'
  'black=23.1.0-1'
  'python3-isort=5.6.4-1'
  'isort=5.6.4-1'
  'python3-flake8=5.0.4-4'
  'flake8=5.0.4-4'
  'pylint=2.16.2-2'
  'python3-ipython=8.5.0-4'
  'ipython3=8.5.0-4'
  'python3-debugpy=1.6.6+ds-1'
  'tox=3.28.0-2'
  'twine=4.0.2-1'
  'pipx=1.1.0-1'
  'python3-poetry=1.3.2+dfsg-3'
)
missing_python_packages=()
for package in "${python_packages[@]}"; do
  grep -Fq "      $package" "$sandbox_dockerfile" \
    || missing_python_packages+=("$package")
done
if [[ "${#missing_python_packages[@]}" -eq 0 ]]; then
  ok 'sandbox image pins every Python development-suite package'
else
  bad "sandbox image is missing Python package pins: ${missing_python_packages[*]}"
fi
if [[ "$(grep -c '^ARG DEBIAN_SNAPSHOT=20261105T082236Z$' "$sandbox_dockerfile" || true)" -eq 1 ]] \
    && [[ "$(grep -c '"deb https://snapshot\.debian\.org/' "$sandbox_dockerfile" || true)" -eq 3 ]] \
    && ! grep -Fq '"deb http://' "$sandbox_dockerfile"; then
  ok 'sandbox image uses the fixed Debian snapshot exclusively over HTTPS'
else
  bad 'sandbox image Debian snapshot or HTTPS source contract drifted'
fi
if [[ "$(grep -c 'Acquire::Retries=0' "$sandbox_dockerfile" || true)" -eq 2 ]] \
    && [[ "$(grep -c 'Acquire::Retries=3' "$sandbox_dockerfile" || true)" -eq 1 ]] \
    && grep -Fq 'dpkg --compare-versions "$(dpkg-query -W -f=' "$sandbox_dockerfile" \
    && grep -Fq "' apt)\" ge 2.6.2;" "$sandbox_dockerfile"; then
  ok 'sandbox image bootstraps the fixed apt client before bounded retries'
else
  bad 'sandbox image apt bootstrap, floor, or retry contract drifted'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all WI#223/WI#537/WI#596 sandbox image tests passed\n'
