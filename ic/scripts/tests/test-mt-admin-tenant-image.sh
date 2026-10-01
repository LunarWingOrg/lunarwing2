#!/usr/bin/env bash
#
# _ensure_tenant_image store-selection contract (WI#229).
#
# The admin (root) store gates *distribution*: when it holds a newer image, the
# tenant store must be refreshed so rebuilt worker/sidecar images actually
# reach tenants. It must NOT gate whether an already-provisioned tenant may
# start a unit it already has the image for — that regression stranded
# LunarVoice after `upgrade-tenant` emptied the admin store, because
# `start_tenant_voice` aborts on this check before reaching its reset-failed +
# start and the unit stayed failed until an operator intervened.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

pass=0; fail=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s (%s)\n' "$1" "$2"; fail=$((fail + 1)); }

# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPTS_DIR/lunarwing-mt-admin.sh"

# Exercise the real function with store probes stubbed. `admin_id`/`tenant_id`
# model `image inspect -f '{{.Id}}'` failing (empty) or returning an ID.
# SAVE_CALLED records whether a save|load distribution was attempted.
#
# NOTE the two different call shapes: the admin-store probe runs `$CONTAINER_RT`
# directly, but the tenant-store probe goes through `_ctr <tenant> <verb...>` —
# the tenant name is $1, so the verb check must start at $2 for that stub.
run_case() {  # <admin_id> <tenant_id> [rootless]
  ADMIN_ID="$1" TENANT_ID="$2"
  # MT_ROOTLESS and CONTAINER_RT are read by _ensure_tenant_image from the
  # sourced mt-admin lib, so this file never references them again.
  # shellcheck disable=SC2034
  MT_ROOTLESS="${3:-true}"
  # shellcheck disable=SC2034
  CONTAINER_RT="podman"
  RETURNED_RC=0
  SAVE_CALLED_MIRROR=false
  SAVE_MARKER="$(mktemp)"
  rm -f "$SAVE_MARKER"

  # The admin-store probe uses $CONTAINER_RT directly. `save` in the
  # `$CONTAINER_RT save | _ctr load` distribution pipeline runs in a pipeline
  # subshell, so it cannot set a parent variable — record it in a file instead.
  podman() {
    case "${1:-}" in
      save) : >"$SAVE_MARKER"; return 0 ;;
    esac
    [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]] || return 0
    [[ -n "$ADMIN_ID" ]] || return 1
    printf '%s' "$ADMIN_ID"
  }
  # The tenant-store probe goes through _ctr <name> <verb...>.
  _ctr() {
    shift   # drop the tenant name
    case "${1:-}" in
      load) return 0 ;;
    esac
    [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]] || return 0
    [[ -n "$TENANT_ID" ]] || return 1
    printf '%s' "$TENANT_ID"
  }
  say() { :; }

  # Run in this shell (not a subshell) so SAVE_CALLED survives the call.
  set +e
  _ensure_tenant_image tenant-a "lunarwing/voice-service:latest"
  local rc=$?
  set -e
  if [[ -e "$SAVE_MARKER" ]]; then SAVE_CALLED_MIRROR=true; fi
  rm -f "$SAVE_MARKER"
  RETURNED_RC="$rc"
}

# ── The regression: admin store empty, tenant already has the image ──────────
run_case "" "abc123"
if [[ "$RETURNED_RC" == "0" ]]; then
  ok "tenant holding the image starts even when the admin store lacks it (WI#229)"
else
  bad "admin-empty + tenant-present must succeed" "rc=$RETURNED_RC"
fi
if [[ "$SAVE_CALLED_MIRROR" == "false" ]]; then
  ok "no distribution attempted when there is no source image to copy"
else
  bad "admin-empty must not attempt save|load" "save was called"
fi

# ── Admin store present, tenant already current: no churn ────────────────────
run_case "same-id" "same-id"
if [[ "$RETURNED_RC" == "0" && "$SAVE_CALLED_MIRROR" == "false" ]]; then
  ok "tenant already on the current image skips redistribution"
else
  bad "current image must short-circuit" "rc=$RETURNED_RC save=$SAVE_CALLED_MIRROR"
fi

# ── Admin newer than tenant: still refresh (the behaviour worth keeping) ─────
run_case "new-id" "old-id"
if [[ "$RETURNED_RC" == "0" && "$SAVE_CALLED_MIRROR" == "true" ]]; then
  ok "admin store holding a newer image still refreshes the tenant store"
else
  bad "stale tenant image must be refreshed" "rc=$RETURNED_RC save=$SAVE_CALLED_MIRROR"
fi

# ── Admin present, tenant absent: distribute ────────────────────────────────
run_case "new-id" ""
if [[ "$RETURNED_RC" == "0" && "$SAVE_CALLED_MIRROR" == "true" ]]; then
  ok "absent tenant image is distributed from the admin store"
else
  bad "missing tenant image must be distributed" "rc=$RETURNED_RC save=$SAVE_CALLED_MIRROR"
fi

# ── Absent everywhere: fail so the caller can tell the operator to build ────
run_case "" ""
if [[ "$RETURNED_RC" == "1" ]]; then
  ok "image absent everywhere fails (caller suggests building first)"
else
  bad "absent-everywhere must fail" "rc=$RETURNED_RC"
fi

# ── Rootful: admin store IS the runtime store ───────────────────────────────
run_case "some-id" "" false
if [[ "$RETURNED_RC" == "0" && "$SAVE_CALLED_MIRROR" == "false" ]]; then
  ok "rootful succeeds on admin-store presence alone"
else
  bad "rootful admin-present must succeed" "rc=$RETURNED_RC save=$SAVE_CALLED_MIRROR"
fi

run_case "" "some-id" false
if [[ "$RETURNED_RC" == "1" ]]; then
  ok "rootful fails when the admin store lacks the image"
else
  bad "rootful admin-absent must fail" "rc=$RETURNED_RC"
fi

# ── WI#271: a failed distribution reports the captured save|load stderr ──────
# Store corruption vs a missing source image vs the conmon-rot class used to
# collapse into one content-free WARNING; the operator could not tell which.
SAY_LOG="$(mktemp)"
: >"$SAY_LOG"
# shellcheck disable=SC2034  # read by _ensure_tenant_image
MT_ROOTLESS=true
# shellcheck disable=SC2034  # read by _ensure_tenant_image
CONTAINER_RT=podman
podman() {
  case "${1:-}" in
    save)
      printf 'Error: writing blob: corrupted layer digest\n' >&2
      return 1
      ;;
  esac
  [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]] || return 0
  printf 'admin-id'
}
_ctr() {
  shift   # drop the tenant name
  case "${1:-}" in
    load) return 0 ;;
  esac
  [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]] || return 0
  return 1   # tenant store lacks the image
}
say() { printf '%s\n' "$1" >>"$SAY_LOG"; }
set +e
_ensure_tenant_image tenant-b "lunarwing-worker-pebble:latest"
rc=$?
set -e
if [[ "$rc" == "1" ]]; then
  ok "failed distribution still returns 1"
else
  bad "failed distribution must return 1" "rc=$rc"
fi
if grep -q "failed to load lunarwing-worker-pebble:latest into tenant-b's store" "$SAY_LOG"; then
  ok "failed distribution keeps the WARNING line"
else
  bad "WARNING line missing" "$(cat "$SAY_LOG")"
fi
if grep -q "corrupted layer digest" "$SAY_LOG"; then
  ok "failed distribution includes the captured save|load stderr (WI#271)"
else
  bad "captured diagnostics missing" "$(cat "$SAY_LOG")"
fi
rm -f "$SAY_LOG"

# ── WI#271 helpers: expected-image inventory, missing detection, states ──────
REGISTRY_FIXTURE="$(mktemp)"
cat >"$REGISTRY_FIXTURE" <<'JSON'
{"version":17,"tenants":{
  "tenant-b":{"enable_voice":true,"vision":{"mode":"local"},"workers":{"pebble":true,"kimi":true}},
  "tenant-c":{"enable_voice":false,"vision":{"mode":"external"},"workers":{}}
}}
JSON
# shellcheck disable=SC2034  # read by the tenant_* registry accessors
PORTS_REGISTRY="$REGISTRY_FIXTURE"

expected_b="$(_tenant_store_expected_images tenant-b)"
want_b="$(printf 'lunarwing/vision-service:latest\tbuild-vision-sidecar\nlunarwing/voice-service:latest\tinstall-voice\nlunarwing-worker-pebble:latest\tbuild-pebble-worker\nlunarwing-worker-kimi:latest\tbuild-kimi-worker')"
if [[ "$expected_b" == "$want_b" ]]; then
  ok "expected images: local vision + enabled voice + selected workers"
else
  bad "expected images mismatch" "$(printf '%q' "$expected_b")"
fi
expected_c="$(_tenant_store_expected_images tenant-c)"
if [[ -z "$expected_c" ]]; then
  ok "expected images: external vision + no voice + no workers yields nothing"
else
  bad "tenant-c must expect no images" "$(printf '%q' "$expected_c")"
fi

# Missing detection: the tenant store holds the vision image only.
declare -A TENANT_IDS=( ["lunarwing/vision-service:latest"]="tid-vision" )
_ctr() {
  shift   # drop the tenant name
  if [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]]; then
    local _id="${TENANT_IDS[${5}]:-}"
    [[ -n "$_id" ]] || return 1
    printf '%s' "$_id"
    return 0
  fi
  return 0
}
missing_b="$(_tenant_store_missing_images tenant-b)" || true
want_missing="$(printf 'lunarwing/voice-service:latest\tinstall-voice\nlunarwing-worker-pebble:latest\tbuild-pebble-worker\nlunarwing-worker-kimi:latest\tbuild-kimi-worker')"
if [[ "$missing_b" == "$want_missing" ]]; then
  ok "missing detection lists exactly the absent expected images"
else
  bad "missing images mismatch" "$(printf '%q' "$missing_b")"
fi
if _tenant_store_missing_images tenant-b >/dev/null; then
  bad "missing images must return non-zero" "returned 0"
else
  ok "missing detection returns non-zero when images are absent"
fi
TENANT_IDS=( ["lunarwing/vision-service:latest"]="v" ["lunarwing/voice-service:latest"]="x" ["lunarwing-worker-pebble:latest"]="y" ["lunarwing-worker-kimi:latest"]="z" )
if _tenant_store_missing_images tenant-b >/dev/null; then
  ok "missing detection returns zero when all expected images are present"
else
  bad "fully stocked store must pass" "returned non-zero"
fi

# Status states: current / STALE / MISSING / no-admin-comparison / rootful.
declare -A ADMIN_IDS=( ["lunarwing/vision-service:latest"]="aid-vision-000000000000" )
podman() {
  if [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]]; then
    local _id="${ADMIN_IDS[${5}]:-}"
    [[ -n "$_id" ]] || return 1
    printf '%s' "$_id"
    return 0
  fi
  return 0
}
# shellcheck disable=SC2034  # read by _tenant_image_state
MT_ROOTLESS=true
TENANT_IDS=( ["lunarwing/vision-service:latest"]="aid-vision-000000000000" )
state="$(_tenant_image_state tenant-b "lunarwing/vision-service:latest")"
[[ "$state" == current\ \(aid-vision-0000000* ]] \
  && ok "image state: tenant ID matching admin reports current" \
  || bad "image state current" "$state"
TENANT_IDS=( ["lunarwing/vision-service:latest"]="tid-old-1111111111111" )
state="$(_tenant_image_state tenant-b "lunarwing/vision-service:latest")"
[[ "$state" == STALE\ \(tenant\ tid-old-1111111111* ]] \
  && ok "image state: differing tenant ID reports STALE" \
  || bad "image state STALE" "$state"
state="$(_tenant_image_state tenant-b "lunarwing-worker-pebble:latest")"
[[ "$state" == "MISSING" ]] \
  && ok "image state: absent tenant image reports MISSING" \
  || bad "image state MISSING" "$state"
TENANT_IDS=( ["lunarwing-worker-kimi:latest"]="tid-kimi-2222222222222" )
state="$(_tenant_image_state tenant-b "lunarwing-worker-kimi:latest")"
[[ "$state" == "present (tid-kimi-2222222222; admin store has no copy to compare)" ]] \
  && ok "image state: tenant-only image reports present without comparison" \
  || bad "image state no-admin" "$state"
# shellcheck disable=SC2034  # read by _tenant_image_state
MT_ROOTLESS=false
state="$(_tenant_image_state tenant-b "lunarwing-worker-kimi:latest")"
[[ "$state" == "present (tid-kimi-2222222222)" ]] \
  && ok "image state: rootful reports presence only" \
  || bad "image state rootful present" "$state"
state="$(_tenant_image_state tenant-b "lunarwing/vision-service:latest")"
TENANT_IDS=()
state="$(_tenant_image_state tenant-b "lunarwing/vision-service:latest")"
[[ "$state" == "MISSING" ]] \
  && ok "image state: rootful absent image reports MISSING" \
  || bad "image state rootful MISSING" "$state"
rm -f "$REGISTRY_FIXTURE"

echo ""
if [[ "$fail" -eq 0 ]]; then
  echo "RESULT: all _ensure_tenant_image tests passed ($pass)"
else
  echo "RESULT: $fail failure(s), $pass passed"
  exit 1
fi
