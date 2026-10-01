#!/usr/bin/env bash
# Health Check: tenant worker/sidecar image stores (WI#271)
#
# Output JSON; exit 0=healthy, 1=degraded, 2=critical.
#
# Rootless-podman tenants keep their own container stores. A failed save|load
# distribution only warned, so a worker image could go MISSING from a tenant's
# store while the already-materialized container kept the smoke green — every
# restart/recreate path would then fail. This check audits, per registry
# tenant, the store presence of the images the tenant is expected to hold
# (local vision sidecar, enabled voice sidecar, selected external workers) and
# compares IDs against the admin (root) store that feeds distribution.
#
# Verdicts: MISSING image -> critical (restart/recreate would fail); STALE
# (tenant ID differs from the admin store's) -> degraded; tenant-present with
# no admin copy to compare -> healthy. Tenants with no expected images are not
# targets. The managed sandbox image is excluded: its runtime is chosen per
# tenant and may differ from the worker runtime probed here.

set -uo pipefail

ENABLED="${HEALTH_WORKER_IMAGES_ENABLED:-auto}"
PROBE_TIMEOUT="${HEALTH_WORKER_IMAGES_TIMEOUT:-5}"
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

if [[ "$ENABLED" != true && "$ENABLED" != auto ]]; then
  printf '{"component":"worker_images","status":"healthy","timestamp":"%s","metrics":{"enabled":false,"reason":"disabled_by_config"},"issues":[]}\n' "$TIMESTAMP"
  exit 0
fi

resolve_registry() {
  local candidate base
  for candidate in \
    "${SELF_HEAL_TENANTS_FILE:-}" \
    "${LUNARWING_TENANTS_FILE:-}" \
    /etc/lunarwing/ports.json; do
    [[ -n "$candidate" && -f "$candidate" ]] || continue
    printf '%s\n' "$candidate"
    return 0
  done
  base="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-}}"
  if [[ -n "$base" && -f "$base/tenants/ports.json" ]]; then
    printf '%s\n' "$base/tenants/ports.json"
    return 0
  fi
  return 1
}

discovery_error() {
  local issue="$1"
  jq -n \
    --arg timestamp "$TIMESTAMP" --arg issue "$issue" \
    '{component:"worker_images",status:"critical",timestamp:$timestamp,
      metrics:{enabled:true,discovery:"ports-registry",target_count:0,instances:[]},issues:[$issue]}'
  exit 2
}

# Images a tenant store is expected to hold, one "<image>\t<build-verb>" pair
# per line. Mirrors mt-admin's _tenant_store_expected_images (kept in sync
# deliberately: the registry is the shared source of truth).
expected_images() {  # <registry> <tenant>
  jq -r --arg t "$2" '
    .tenants[$t] as $x |
    (if ($x.vision.mode // "local") == "external" then empty
     else "lunarwing/vision-service:latest\tbuild-vision-sidecar" end),
    (if $x.enable_voice == true
     then "lunarwing/voice-service:latest\tinstall-voice" else empty end),
    (($x.workers // {}) | to_entries[]
      | select(.value == true)
      | select(.key == "pebble" or .key == "opencode" or .key == "opencode2" or .key == "kimi")
      | "lunarwing-worker-\(.key):latest\tbuild-\(.key)-worker")
  ' "$1"
}

# Probe one image in the admin (root) store / a tenant's rootless store.
admin_image_id() {  # <image>
  timeout "$PROBE_TIMEOUT" podman image inspect -f '{{.Id}}' "$1" 2>/dev/null || true
}
tenant_image_id() {  # <tenant> <uid> <home> <image>
  timeout "$PROBE_TIMEOUT" sudo -u "$1" \
    env HOME="$3" XDG_RUNTIME_DIR="/run/user/$2" \
    podman image inspect -f '{{.Id}}' "$4" 2>/dev/null || true
}

registry=""
if ! registry="$(resolve_registry)"; then
  jq -n \
    --arg timestamp "$TIMESTAMP" \
    '{component:"worker_images",status:"healthy",timestamp:$timestamp,
      metrics:{enabled:false,reason:"not_configured",discovery:"single",target_count:0,instances:[]},issues:[]}'
  exit 0
fi

jq -e 'type == "object" and ((.tenants // {}) | type == "object")' \
  "$registry" >/dev/null 2>&1 \
  || discovery_error "ports registry is not valid tenant JSON"

invalid="$(jq -r '
  (.tenants // {}) | to_entries[]
  | select(
      ((.value | has("enable_voice")) and (.value.enable_voice | type != "boolean"))
      or ((.value.workers // {}) | to_entries | any(.value | type != "boolean"))
    )
  | .key
' "$registry" 2>/dev/null)" \
  || discovery_error "failed to inspect tenant worker/voice enablement flags"
if [[ -n "$invalid" ]]; then
  invalid="$(paste -sd, <<<"$invalid")"
  discovery_error "tenant has non-boolean worker/voice enablement state: $invalid"
fi

if ! command -v podman >/dev/null 2>&1 || ! command -v sudo >/dev/null 2>&1; then
  if [[ "$ENABLED" == true ]]; then
    discovery_error "podman and sudo are required to probe tenant image stores"
  fi
  jq -n \
    --arg timestamp "$TIMESTAMP" \
    '{component:"worker_images",status:"healthy",timestamp:$timestamp,
      metrics:{enabled:false,reason:"podman_absent",discovery:"ports-registry",target_count:0,instances:[]},issues:[]}'
  exit 0
fi

rank() {
  case "$1" in critical) printf 3 ;; degraded) printf 2 ;; healthy) printf 1 ;; *) printf 0 ;; esac
}

# Probe one tenant's store against its expected images; prints the instance JSON.
probe_tenant() {  # <tenant>
  local tenant="$1" uid home image hint tid aid state
  local status=healthy
  local images='[]' issues='[]'
  uid="$(id -u "$tenant" 2>/dev/null)" || uid=""
  home="$(getent passwd "$tenant" 2>/dev/null | cut -d: -f6)"
  while IFS=$'\t' read -r image hint; do
    [[ -n "$image" ]] || continue
    tid="" aid=""
    if [[ -n "$uid" && -n "$home" ]]; then
      tid="$(tenant_image_id "$tenant" "$uid" "$home" "$image")"
    fi
    aid="$(admin_image_id "$image")"
    if [[ -z "$tid" ]]; then
      state=missing
      status=critical
      issues="$(jq -c --arg m "image missing from ${tenant}'s store: $image (recover: mt-admin $hint, then start-tenant $tenant re-distributes)" \
        '. + [$m]' <<<"$issues")"
    elif [[ -n "$aid" && "$tid" != "$aid" ]]; then
      state=stale
      (( $(rank "$status") < 2 )) && status=degraded
      issues="$(jq -c --arg m "stale image in ${tenant}'s store: $image (store ${tid:0:19} != admin ${aid:0:19}); next start-tenant recreates onto the current image" \
        '. + [$m]' <<<"$issues")"
    elif [[ -z "$aid" ]]; then
      state=present-no-admin-copy
    else
      state=current
    fi
    images="$(jq -c --arg image "$image" --arg state "$state" \
      --arg tid "${tid:0:19}" --arg aid "${aid:0:19}" \
      '. + [{image:$image,state:$state,tenant_id:$tid,admin_id:$aid}]' <<<"$images")"
  done < <(expected_images "$registry" "$tenant")
  jq -cn \
    --arg tenant "$tenant" --arg status "$status" \
    --argjson images "$images" --argjson issues "$issues" \
    '{tenant:$tenant,status:$status,images:$images,issues:$issues}'
}

overall=healthy
exit_code=0
instances='[]'
all_issues='[]'
target_count=0
# Fleet sweep deadline (same partial-results contract as health-lunarvoice.sh,
# WI#180 F4): stop starting new probes, keep completed observations, mark the
# rest unprobed; incomplete coverage degrades at worst, never masks. 0 disables.
SWEEP_DEADLINE_SECS="${HEALTH_WORKER_IMAGES_SWEEP_DEADLINE_SECS:-22}"
sweep_incomplete=false
unprobed_count=0
while IFS= read -r tenant; do
  [[ -n "$tenant" ]] || continue
  # Tenants with no expected images are not targets.
  expected="$(expected_images "$registry" "$tenant")"
  [[ -n "$expected" ]] || continue
  if [ "$SWEEP_DEADLINE_SECS" != "0" ] && [ "$SECONDS" -ge "$SWEEP_DEADLINE_SECS" ]; then
    sweep_incomplete=true
    unprobed_count=$((unprobed_count + 1))
    instances="$(jq -c --arg tenant "$tenant" \
      '. + [{tenant:$tenant,status:"unprobed",issues:["probe skipped: sweep deadline"]}]' \
      <<<"$instances")"
    continue
  fi
  target_count=$((target_count + 1))
  instance="$(probe_tenant "$tenant")"
  instance_status="$(jq -r '.status' <<<"$instance")"
  if (( $(rank "$instance_status") > $(rank "$overall") )); then
    overall="$instance_status"
  fi
  instances="$(jq -c --argjson instance "$instance" '. + [$instance]' <<<"$instances")"
  all_issues="$(jq -c --argjson more "$(jq '.issues' <<<"$instance")" '. + $more' <<<"$all_issues")"
done < <(jq -r '(.tenants // {}) | keys[]' "$registry")

if [[ "$target_count" -eq 0 && "$sweep_incomplete" != true ]]; then
  jq -n \
    --arg timestamp "$TIMESTAMP" \
    '{component:"worker_images",status:"healthy",timestamp:$timestamp,
      metrics:{enabled:false,reason:"no_expected_images",discovery:"ports-registry",target_count:0,instances:[]},issues:[]}'
  exit 0
fi

case "$overall" in critical) exit_code=2 ;; degraded) exit_code=1 ;; esac

# Incomplete coverage is a monitoring failure: degrade at worst, never mask a
# worse instance verdict (WI#180 F4).
if [ "$sweep_incomplete" = "true" ]; then
  all_issues="$(jq -c --arg msg "target sweep incomplete: $unprobed_count target(s) unprobed (deadline ${SWEEP_DEADLINE_SECS}s); results partial" \
    '. + [$msg]' <<<"$all_issues")"
  if (( $(rank "$overall") < 2 )); then
    overall="degraded"
    exit_code=1
  fi
fi

jq -n \
  --arg timestamp "$TIMESTAMP" --arg status "$overall" \
  --argjson instances "$instances" --argjson issues "$all_issues" \
  '{component:"worker_images",status:$status,timestamp:$timestamp,
    metrics:{enabled:true,discovery:"ports-registry",target_count:($instances|length),instances:$instances},issues:$issues}'
exit "$exit_code"
