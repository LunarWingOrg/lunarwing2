#!/usr/bin/env bash
# Ordinary reconciliation must not turn a daemon upgrade into a DB upgrade.
# shellcheck disable=SC2034  # fixture globals are consumed by sourced mt-admin functions
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0
PG_IMAGE=docker.io/pgvector/pgvector:pg16@sha256:new
old_image=docker.io/pgvector/pgvector:pg16@sha256:old
quadlet="$fixture/lunarwing-pg-alice.container"
ensure_container_runtime() { :; }
ensure_init_system() { INIT_SYSTEM=systemd; }
_ensure_container_health_interval() { CONTAINER_HEALTH_INTERVAL_SEC=30; }
tenant_quadlet_dir() { printf '%s\n' "$fixture"; }
ports_get() { printf '20000\n'; }
tenant_pg_password() { printf 'quadlet-fixture-only\n'; }
tenant_mkdir_p() { mkdir -p "$2"; }
tenant_cat() { cat -- "$2"; }
_write_systemd_unit() { cat >"$2"; }
_wait_user_manager() { :; }
podman_supports_quadlet() { return 0; }
_systemctl_user() { printf '%s\n' "$*" >>"$fixture/events"; }
_ctr_inspect() { return 1; }
_root_store_pg_container_present() { return 1; }
_ctr() { [[ "$*" == *pg_isready* ]]; }
MT_ROOTLESS=true
CONTAINER_RT=podman
assert_image() {
  if grep -Fxq "Image=$2" "$quadlet"; then printf 'PASS: %s\n' "$1"
  else printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); fi
}
render_pg_quadlet alice
assert_image 'fresh tenant uses reviewed default' "$PG_IMAGE"
sed -i "s|^Image=.*|Image=$old_image|" "$quadlet"
start_tenant_postgres alice
assert_image 'ordinary Quadlet start preserves previous digest' "$old_image"
render_pg_quadlet alice
assert_image 'ordinary unit reconciliation preserves previous digest' "$old_image"
sed -i 's|^Image=.*|Image=docker.io/pgvector/pgvector:pg16|' "$quadlet"
start_tenant_postgres alice
assert_image 'legacy floating reference is retained, not silently switched to pin' 'docker.io/pgvector/pgvector:pg16'
render_pg_quadlet alice "$PG_IMAGE"
assert_image 'explicit refresh render selects reviewed digest' "$PG_IMAGE"
start_tenant_postgres alice
assert_image 'subsequent ordinary start retains refreshed pin' "$PG_IMAGE"
printf 'Image=\n' >"$quadlet"
if (render_pg_quadlet alice) >"$fixture/output" 2>&1; then
  printf 'FAIL: ambiguous existing image must fail closed\n'; failures=$((failures + 1))
else
  printf 'PASS: ambiguous existing image fails closed\n'
fi
printf 'Image=%s\nImage=%s\n' "$old_image" "$PG_IMAGE" >"$quadlet"
if (render_pg_quadlet alice) >"$fixture/output" 2>&1; then
  printf 'FAIL: multiple existing images must fail closed\n'; failures=$((failures + 1))
else
  printf 'PASS: multiple existing images fail closed\n'
fi
[[ "$failures" -eq 0 ]]
