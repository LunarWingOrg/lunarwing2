#!/usr/bin/env bash
#
# WI#271: worker recreation contracts carry the tenant store's image ID.
#
# A successful image refresh into the tenant store never bounced the RUNNING
# worker container: _container_config_changed hashed only the config file and
# a static launch-contract string, so a worker could pin an arbitrarily old
# image while the store held the current tag. The contract now folds in
# `image=<tenant-store-id>` (matching the vision/voice sidecar contracts), on
# both the Quadlet and the imperative creation paths.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fail=0; pass=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s (%s)\n' "$1" "$2"; fail=$((fail + 1)); }

# ── _worker_recreate_contract string shapes ──────────────────────────────────
[[ "$(_worker_recreate_contract "" "base")" == "base" ]] \
  && ok "empty image ID leaves the base contract unchanged" \
  || bad "empty ID" "$(_worker_recreate_contract "" "base")"
[[ "$(_worker_recreate_contract "sha256:abc" "base")" == "base;image=sha256:abc" ]] \
  && ok "image ID appends to a non-empty base contract" \
  || bad "append" "$(_worker_recreate_contract "sha256:abc" "base")"
[[ "$(_worker_recreate_contract "sha256:abc" "")" == "image=sha256:abc" ]] \
  && ok "image ID alone forms the quadlet-side contract" \
  || bad "image-only" "$(_worker_recreate_contract "sha256:abc" "")"

# ── Behavioral: start_tenant_pebble passes the image contract on both paths ──
fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
mkdir -p "$fixture/quadlets" "$fixture/env"

say() { :; }
die() { printf 'die: %s\n' "$*" >&2; exit 1; }
ensure_container_runtime() {
  # Globals consumed by sourced lifecycle helpers.
  # shellcheck disable=SC2034
  CONTAINER_RT="podman"
  # shellcheck disable=SC2034
  MT_ROOTLESS="true"
}
ensure_init_system() { :; }
ports_get() { [[ "$2" == "pebble_wss" ]] && printf '19091\n'; }
_ensure_tenant_image() { return 0; }
_tenant_store_image_id() { printf 'sha256:testimg'; }
_register_worker_unit() { :; }
_wait_user_manager() { :; }
tenant_quadlet_dir() { printf '%s/quadlets\n' "$fixture"; }
tenant_env_dir() { printf '%s/env\n' "$fixture"; }
tenant_lw_root() { printf '%s/lw\n' "$fixture"; }
tenant_run_dir() { printf '%s/run\n' "$fixture"; }
render_worker_quadlet() { : >"$(tenant_quadlet_dir "$1")/lunarwing-pebble-$1.container"; }
_systemctl_user() { return 0; }

CHANGED_ARGS=""
STORED_ARGS=""
_container_config_changed() { CHANGED_ARGS="$*"; return "${CHANGED_RC:-1}"; }
_store_container_hash() { STORED_ARGS="$*"; }

# Leg 1: Quadlet path — the .container file hash alone used to decide
# recreation; the image contract must now ride along as the 4th argument.
# shellcheck disable=SC2034  # read by the sourced start path
INIT_SYSTEM="systemd"
podman_supports_quadlet() { return 0; }
CHANGED_RC=0   # report "changed" so the recreate + store path runs
_ctr() { shift; return 0; }

start_tenant_pebble alice
quadlet_file_arg="$fixture/quadlets/lunarwing-pebble-alice.container"
if [[ "$CHANGED_ARGS" == "alice lunarwing-pebble-alice $quadlet_file_arg image=sha256:testimg" ]]; then
  ok "quadlet recreation gate receives the image contract"
else
  bad "quadlet gate args" "$CHANGED_ARGS"
fi
if [[ "$STORED_ARGS" == "alice lunarwing-pebble-alice $quadlet_file_arg image=sha256:testimg" ]]; then
  ok "quadlet stored hash records the image contract"
else
  bad "quadlet store args" "$STORED_ARGS"
fi

# Leg 2: imperative path (systemd without Quadlet support) — an existing,
# running container whose config is unchanged must hash-check against the
# image-augmented contract and re-store it.
# shellcheck disable=SC2034  # read by the sourced start path
INIT_SYSTEM="systemd"
podman_supports_quadlet() { return 1; }
CHANGED_RC=1   # unchanged: exercise the already-running store path
CHANGED_ARGS=""
STORED_ARGS=""
_ctr() {
  shift   # drop the tenant name
  case "${1:-}" in
    inspect)
      if [[ "${2:-}" == "-f" ]]; then printf 'true\n'; fi
      return 0
      ;;
  esac
  return 0
}

start_tenant_pebble alice
want_contract="${PEBBLE_ROOTLESS_LAUNCH_CONTRACT};image=sha256:testimg"
if [[ "$CHANGED_ARGS" == "alice lunarwing-pebble-alice $fixture/env/lunarwing.env $want_contract" ]]; then
  ok "imperative recreation gate receives the image-augmented contract"
else
  bad "imperative gate args" "$CHANGED_ARGS"
fi
if [[ "$STORED_ARGS" == "alice lunarwing-pebble-alice $fixture/env/lunarwing.env $want_contract" ]]; then
  ok "imperative stored hash records the image-augmented contract"
else
  bad "imperative store args" "$STORED_ARGS"
fi

# Leg 3: an uninspectable image degrades to the pre-WI#271 static contract
# instead of breaking the start path.
_tenant_store_image_id() { :; }
CHANGED_ARGS=""
STORED_ARGS=""
start_tenant_pebble alice
if [[ "$CHANGED_ARGS" == "alice lunarwing-pebble-alice $fixture/env/lunarwing.env $PEBBLE_ROOTLESS_LAUNCH_CONTRACT" ]]; then
  ok "uninspectable image falls back to the static contract"
else
  bad "fallback gate args" "$CHANGED_ARGS"
fi

echo ""
if [[ "$fail" -eq 0 ]]; then
  echo "RESULT: all worker image-contract tests passed ($pass)"
else
  echo "RESULT: $fail failure(s), $pass passed"
  exit 1
fi
