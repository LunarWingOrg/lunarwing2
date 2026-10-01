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

# ── WI#271: a failed distribution quotes the failing side's stderr ──────────
# The generic "failed to load" warning must name the failing half of the
# save|load pipeline and include its first stderr lines — a missing admin-side
# image, store corruption, and EPIPE from a load-side early death are
# different incidents and must not collapse into one bare warning.
run_case_dist_fail() {  # <save_stderr> <load_stderr>
  ADMIN_ID="new-id" TENANT_ID="old-id"
  # shellcheck disable=SC2034  # read by _ensure_tenant_image from the sourced lib
  MT_ROOTLESS=true
  # shellcheck disable=SC2034  # read by _ensure_tenant_image from the sourced lib
  CONTAINER_RT="podman"
  SAVE_ERR_TEXT="$1" LOAD_ERR_TEXT="$2"
  SAY_LOG="$(mktemp)"

  podman() {
    case "${1:-}" in
      save)
        if [[ -n "$SAVE_ERR_TEXT" ]]; then
          printf '%s\n' "$SAVE_ERR_TEXT" >&2
          return 1
        fi
        return 0
        ;;
    esac
    [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]] || return 0
    [[ -n "$ADMIN_ID" ]] || return 1
    printf '%s' "$ADMIN_ID"
  }
  # The tenant-store probe and the load half go through _ctr <name> <verb...>.
  _ctr() {
    shift
    case "${1:-}" in
      load)
        if [[ -n "$LOAD_ERR_TEXT" ]]; then
          printf '%s\n' "$LOAD_ERR_TEXT" >&2
          return 1
        fi
        return 0
        ;;
    esac
    [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]] || return 0
    [[ -n "$TENANT_ID" ]] || return 1
    printf '%s' "$TENANT_ID"
  }
  say() { printf '%s\n' "$*" >>"$SAY_LOG"; }

  set +e
  _ensure_tenant_image tenant-a "lunarwing-worker-pebble:latest"
  local rc=$?
  set -e
  RETURNED_RC="$rc"
}

run_case_dist_fail "Error: admin store corrupted while reading image" ""
if [[ "$RETURNED_RC" == "1" ]] \
    && grep -q "WARNING: failed to load lunarwing-worker-pebble:latest into tenant-a's store (save: Error: admin store corrupted while reading image)" "$SAY_LOG"; then
  ok "save-side failure quotes the admin store's stderr (WI#271)"
else
  bad "save-side failure must quote save stderr" "rc=$RETURNED_RC out=$(cat "$SAY_LOG")"
fi

run_case_dist_fail "" "Error: payload does not match its manifest digest"
if [[ "$RETURNED_RC" == "1" ]] \
    && grep -q "WARNING: failed to load lunarwing-worker-pebble:latest into tenant-a's store (load: Error: payload does not match its manifest digest)" "$SAY_LOG"; then
  ok "load-side failure quotes the tenant store's stderr (WI#271)"
else
  bad "load-side failure must quote load stderr" "rc=$RETURNED_RC out=$(cat "$SAY_LOG")"
fi
rm -f "$SAY_LOG"

echo ""
if [[ "$fail" -eq 0 ]]; then
  echo "RESULT: all _ensure_tenant_image tests passed ($pass)"
else
  echo "RESULT: $fail failure(s), $pass passed"
  exit 1
fi
