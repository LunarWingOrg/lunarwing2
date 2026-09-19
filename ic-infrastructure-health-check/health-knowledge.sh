#!/usr/bin/env bash
# Health Check: Knowledge publication, sync, sweep, and index repair (WI#180 F8)
#
# These background paths can stop progressing while the daemon remains active.
# The check is enablement-aware and monitor-only: it diagnoses and escalates,
# it never restarts anything, and inactivity is not automatically failure.
#
# Signals (all file-based, no daemon restart needed):
#   - knowledge_sync_state.json:  last_attempt_at / last_success_at,
#     consecutive_failures, last_error_class, notes_deferred_last_run
#   - knowledge_sweep_state.json: last_run (sweep cadence)
#
# Status semantics:
#   - sync disabled (no state file, no published source)  -> healthy/disabled
#   - consecutive_failures > 0                            -> degraded
#   - consecutive_failures >= critical threshold          -> critical
#   - deferred notes present                              -> degraded
#   - sweep enabled but never run / cadence overdue       -> degraded
#   - state file malformed                                -> degraded (monitoring gap)
#
# Output: JSON to stdout; exit 0=healthy, 1=degraded, 2=critical.
#
# Env vars:
#   HEALTH_KNOWLEDGE_ENABLED           (default: auto)  — false disables
#   HEALTH_KNOWLEDGE_SYNC_FAIL_DEGRADED (default: 1)
#   HEALTH_KNOWLEDGE_SYNC_FAIL_CRITICAL (default: 5)
#   HEALTH_KNOWLEDGE_SWEEP_OVERDUE_HOURS (default: 72)
#   SELF_HEAL_TENANTS_FILE / LUNARWING_TENANTS_FILE     — ports registry path
#   LUNARWING_BASE_DIR / IRONCLAW_BASE_DIR              — single-node base dir
set -uo pipefail

FAIL_DEGRADED="${HEALTH_KNOWLEDGE_SYNC_FAIL_DEGRADED:-1}"
[[ "$FAIL_DEGRADED" =~ ^[0-9]+$ ]] || FAIL_DEGRADED=1
FAIL_CRITICAL="${HEALTH_KNOWLEDGE_SYNC_FAIL_CRITICAL:-5}"
[[ "$FAIL_CRITICAL" =~ ^[0-9]+$ ]] || FAIL_CRITICAL=5
SWEEP_OVERDUE_HOURS="${HEALTH_KNOWLEDGE_SWEEP_OVERDUE_HOURS:-72}"
[[ "$SWEEP_OVERDUE_HOURS" =~ ^[0-9]+$ ]] || SWEEP_OVERDUE_HOURS=72
ENABLED="${HEALTH_KNOWLEDGE_ENABLED:-auto}"
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
now="$(date +%s)"

if [[ "$ENABLED" != true && "$ENABLED" != auto ]]; then
  printf '{"component":"knowledge","status":"healthy","timestamp":"%s","metrics":{"enabled":false,"reason":"disabled_by_config"},"issues":[]}\n' "$TIMESTAMP"
  exit 0
fi

resolve_registry() {
  local candidate base
  for candidate in \
    "${SELF_HEAL_TENANTS_FILE:-}" \
    "${LUNARWING_TENANTS_FILE:-}" \
    "${HEALTH_KNOWLEDGE_TENANTS_FILE:-/etc/lunarwing/ports.json}"; do
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

# epoch from an ISO-8601 timestamp; empty on parse failure
iso_epoch() {
  local ts="$1"
  [[ -n "$ts" ]] || return 0
  date -d "$ts" +%s 2>/dev/null || true
}

# Inspect one tenant's knowledge state files. Sets globals: p_sync_present
# p_sync_failures p_sync_error_class p_sync_deferred p_sync_last_success_age
# p_sync_last_attempt_age p_sweep_present p_sweep_last_run_age p_issues.
probe_one() {  # <tenant> <state-dir>
  local tenant="$1" dir="$2" sync_file sweep_file
  p_sync_present=false; p_sync_failures=0; p_sync_error_class=""
  p_sync_deferred=0; p_sync_last_success_age=-1; p_sync_last_attempt_age=-1
  p_sweep_present=false; p_sweep_last_run_age=-1
  p_malformed=false
  p_issues=()

  sync_file="$dir/knowledge_sync_state.json"
  if [[ -r "$sync_file" ]]; then
    if ! jq -e . >/dev/null 2>&1 "$sync_file"; then
      p_malformed=true
      p_issues+=("knowledge sync state malformed: $sync_file")
    else
      p_sync_present=true
      p_sync_failures="$(jq -r '.consecutive_failures // 0' "$sync_file" 2>/dev/null || echo 0)"
      p_sync_error_class="$(jq -r '.last_error_class // ""' "$sync_file" 2>/dev/null || true)"
      p_sync_deferred="$(jq -r '.notes_deferred_last_run // 0' "$sync_file" 2>/dev/null || echo 0)"
      local success_epoch attempt_epoch
      success_epoch="$(iso_epoch "$(jq -r '.last_success_at // ""' "$sync_file" 2>/dev/null || true)")"
      attempt_epoch="$(iso_epoch "$(jq -r '.last_attempt_at // ""' "$sync_file" 2>/dev/null || true)")"
      if [[ "$success_epoch" =~ ^[0-9]+$ ]]; then
        p_sync_last_success_age=$(( now - success_epoch ))
      fi
      if [[ "$attempt_epoch" =~ ^[0-9]+$ ]]; then
        p_sync_last_attempt_age=$(( now - attempt_epoch ))
      fi
    fi
  fi

  sweep_file="$dir/knowledge_sweep_state.json"
  if [[ -r "$sweep_file" ]]; then
    if ! jq -e . >/dev/null 2>&1 "$sweep_file"; then
      p_malformed=true
      p_issues+=("knowledge sweep state malformed: $sweep_file")
    else
      p_sweep_present=true
      local sweep_epoch
      sweep_epoch="$(iso_epoch "$(jq -r '.last_run // ""' "$sweep_file" 2>/dev/null || true)")"
      if [[ "$sweep_epoch" =~ ^[0-9]+$ ]]; then
        p_sweep_last_run_age=$(( now - sweep_epoch ))
      fi
    fi
  fi
}

# Fleet sweep deadline (WI#180 F4 contract): file reads are cheap, but a huge
# tenant list must still land inside the driver budget. 0 disables.
SWEEP_DEADLINE_SECS="${HEALTH_KNOWLEDGE_SWEEP_DEADLINE_SECS:-22}"
sweep_incomplete=false
unprobed_count=0

instances='[]'
issues='[]'
overall="healthy"
exit_code=0
discovery="single"
targets=()

if registry="$(resolve_registry)"; then
  discovery="ports-registry"
  if ! jq -e 'type == "object" and ((.tenants // {}) | type == "object")' \
      "$registry" >/dev/null 2>&1; then
    printf '{"component":"knowledge","status":"degraded","timestamp":"%s","metrics":{"enabled":true,"discovery":"ports-registry","target_count":0,"instances":[]},"issues":["ports registry is not valid tenant JSON"]}\n' "$TIMESTAMP"
    exit 1
  fi
  while IFS=$'\t' read -r tenant user; do
    [[ -n "$tenant" ]] || continue
    if [[ -n "${HEALTH_KNOWLEDGE_STATE_ROOT:-}" ]]; then
      targets+=("$tenant|$HEALTH_KNOWLEDGE_STATE_ROOT")
    else
      targets+=("$tenant|/home/$user/lunarwing/state")
    fi
  done < <(jq -r '(.tenants // {}) | to_entries[] | "\(.key)\t\(.value.user // .key)"' "$registry" 2>/dev/null)
  if [[ ${#targets[@]} -eq 0 ]]; then
    printf '{"component":"knowledge","status":"healthy","timestamp":"%s","metrics":{"enabled":false,"reason":"no_tenants","discovery":"ports-registry","target_count":0,"instances":[]},"issues":[]}\n' "$TIMESTAMP"
    exit 0
  fi
else
  base="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}"
  targets+=("default|$base")
fi

for target in "${targets[@]}"; do
  if [[ "$SWEEP_DEADLINE_SECS" != "0" ]] && [[ "$SECONDS" -ge "$SWEEP_DEADLINE_SECS" ]]; then
    sweep_incomplete=true
    unprobed_count=$((unprobed_count + 1))
    instances="$(jq -c --arg tenant "${target%%|*}" \
      '. + [{tenant:$tenant,status:"unprobed",issues:["probe skipped: sweep deadline"]}]' \
      <<<"$instances")"
    continue
  fi
  tenant="${target%%|*}"; dir="${target#*|}"
  probe_one "$tenant" "$dir"

  inst_status="healthy"; inst_exit=0
  if [[ "$p_malformed" == "true" ]]; then
    inst_status="degraded"; inst_exit=1
  elif [[ "$p_sync_failures" =~ ^[0-9]+$ ]] && [[ "$p_sync_failures" -ge "$FAIL_CRITICAL" ]]; then
    inst_status="critical"; inst_exit=2
    p_issues+=("knowledge sync failing: ${p_sync_failures} consecutive failures (last: ${p_sync_error_class:-unknown})")
  elif [[ "$p_sync_failures" =~ ^[0-9]+$ ]] && [[ "$p_sync_failures" -ge "$FAIL_DEGRADED" ]]; then
    inst_status="degraded"; inst_exit=1
    p_issues+=("knowledge sync failing: ${p_sync_failures} consecutive failures (last: ${p_sync_error_class:-unknown})")
  elif [[ "$p_sync_deferred" =~ ^[0-9]+$ ]] && [[ "$p_sync_deferred" -gt 0 ]]; then
    inst_status="degraded"; inst_exit=1
    p_issues+=("knowledge sync deferred notes present: ${p_sync_deferred}")
  elif [[ "$p_sweep_present" == "true" && "$p_sweep_last_run_age" =~ ^[0-9]+$ ]] \
      && [[ "$p_sweep_last_run_age" -gt $(( SWEEP_OVERDUE_HOURS * 3600 )) ]]; then
    inst_status="degraded"; inst_exit=1
    p_issues+=("knowledge sweep overdue: last run ${p_sweep_last_run_age}s ago")
  fi
  if [[ "$inst_exit" -gt "$exit_code" ]]; then
    exit_code="$inst_exit"
    overall="$([[ "$inst_exit" -eq 2 ]] && echo critical || echo degraded)"
  fi

  iss_json="[]"
  [[ ${#p_issues[@]} -eq 0 ]] || iss_json="$(printf '%s\n' "${p_issues[@]}" | jq -R . | jq -s .)"
  inst="$(jq -n \
    --arg tenant "$tenant" \
    --arg status "$inst_status" \
    --arg sync_present "$p_sync_present" \
    --arg sweep_present "$p_sweep_present" \
    --arg sync_error_class "$p_sync_error_class" \
    --argjson sync_failures "$([[ "$p_sync_failures" =~ ^[0-9]+$ ]] && echo "$p_sync_failures" || echo 0)" \
    --argjson sync_deferred "$([[ "$p_sync_deferred" =~ ^[0-9]+$ ]] && echo "$p_sync_deferred" || echo 0)" \
    --argjson sync_last_success_age "$([[ "$p_sync_last_success_age" =~ ^[0-9]+$ ]] && echo "$p_sync_last_success_age" || echo -1)" \
    --argjson sync_last_attempt_age "$([[ "$p_sync_last_attempt_age" =~ ^[0-9]+$ ]] && echo "$p_sync_last_attempt_age" || echo -1)" \
    --argjson sweep_last_run_age "$([[ "$p_sweep_last_run_age" =~ ^[0-9]+$ ]] && echo "$p_sweep_last_run_age" || echo -1)" \
    --argjson issues "$iss_json" \
    '{tenant:$tenant,status:$status,sync_state_present:($sync_present=="true"),sweep_state_present:($sweep_present=="true"),
      sync_consecutive_failures:$sync_failures,sync_last_error_class:$sync_error_class,
      sync_deferred_notes:$sync_deferred,sync_last_success_age_seconds:$sync_last_success_age,
      sync_last_attempt_age_seconds:$sync_last_attempt_age,sweep_last_run_age_seconds:$sweep_last_run_age,
      issues:$issues}')"
  instances="$(jq -c --argjson item "$inst" '. + [$item]' <<<"$instances")"
  issues="$(jq -c --argjson more "$iss_json" '. + $more' <<<"$issues")"
done

if [[ "$sweep_incomplete" == "true" ]]; then
  issues="$(jq -c --arg msg "target sweep incomplete: $unprobed_count target(s) unprobed (deadline ${SWEEP_DEADLINE_SECS}s); results partial" \
    '. + [$msg]' <<<"$issues")"
  if [[ "$exit_code" -lt 1 ]]; then
    overall="degraded"; exit_code=1
  fi
fi

jq -n \
  --arg status "$overall" \
  --arg timestamp "$TIMESTAMP" \
  --arg discovery "$discovery" \
  --argjson instances "$instances" \
  --argjson issues "$issues" \
  '{component:"knowledge",status:$status,timestamp:$timestamp,
    metrics:{enabled:true,discovery:$discovery,target_count:($instances|length),instances:$instances},issues:$issues}'
exit "$exit_code"
