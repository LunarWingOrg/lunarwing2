#!/usr/bin/env bash
# Health Check: Knowledge publication, sync, sweep, and index repair (WI#180 F8)
#
# These background paths can stop progressing while the daemon remains active.
# The check is enablement-aware and monitor-only: it diagnoses and escalates,
# it never restarts anything, and inactivity is not automatically failure.
#
# Enablement contract (MR !1041 review finding 6): the daemon's actual feature
# configuration is read from the tenant env file (KNOWLEDGE_SYNC_ENABLED,
# default false; KNOWLEDGE_SWEEP_ENABLED, default true; plus the cadence
# hours), and every classification depends on it:
#   - feature disabled            -> its state (even stale/malformed) is inert:
#                                    healthy, enabled=false surfaced
#   - enabled, never observed     -> healthy inside the startup grace period
#                                    (env-file mtime proxies "enabled since"),
#                                    degraded once past grace
#   - enabled, failing            -> degraded / critical by failure thresholds
#   - enabled, not attempting    -> degraded once past cadence x overdue
#                                    multiplier (the loop is not running)
#   - sweep overdue               -> degraded past cadence x overdue multiplier
#
# Signals (all file-based, no daemon restart needed):
#   - knowledge_sync_state.json:  last_attempt_at / last_success_at,
#     consecutive_failures, last_error_class, notes_deferred_last_run
#   - knowledge_sweep_state.json: last_run (sweep cadence)
#
# Status semantics:
#   - malformed state of an ENABLED feature -> degraded (monitoring gap)
#   - deferred notes present (enabled sync) -> degraded
#
# Output: JSON to stdout; exit 0=healthy, 1=degraded, 2=critical.
#
# Env vars:
#   HEALTH_KNOWLEDGE_ENABLED           (default: auto)  — false disables
#   HEALTH_KNOWLEDGE_SYNC_FAIL_DEGRADED (default: 1)
#   HEALTH_KNOWLEDGE_SYNC_FAIL_CRITICAL (default: 5)
#   HEALTH_KNOWLEDGE_SWEEP_OVERDUE_HOURS — absolute override; default: cadence
#     x HEALTH_KNOWLEDGE_SWEEP_OVERDUE_MULTIPLIER (3) = 72h at the 24h default
#   HEALTH_KNOWLEDGE_SYNC_OVERDUE_MULTIPLIER (default: 3)
#   HEALTH_KNOWLEDGE_STARTUP_GRACE_HOURS (default: 24; 0 = no grace)
#   HEALTH_KNOWLEDGE_ENV_FILE          — override the tenant env file path
#   SELF_HEAL_TENANTS_FILE / LUNARWING_TENANTS_FILE     — ports registry path
#   LUNARWING_BASE_DIR / IRONCLAW_BASE_DIR              — single-node base dir
set -uo pipefail

FAIL_DEGRADED="${HEALTH_KNOWLEDGE_SYNC_FAIL_DEGRADED:-1}"
[[ "$FAIL_DEGRADED" =~ ^[0-9]+$ ]] || FAIL_DEGRADED=1
FAIL_CRITICAL="${HEALTH_KNOWLEDGE_SYNC_FAIL_CRITICAL:-5}"
[[ "$FAIL_CRITICAL" =~ ^[0-9]+$ ]] || FAIL_CRITICAL=5
# Absolute overdue override (legacy knob); empty = derive from cadence below.
SWEEP_OVERDUE_HOURS="${HEALTH_KNOWLEDGE_SWEEP_OVERDUE_HOURS:-}"
[[ "$SWEEP_OVERDUE_HOURS" =~ ^[0-9]+$ ]] || SWEEP_OVERDUE_HOURS=""
SWEEP_OVERDUE_MULTIPLIER="${HEALTH_KNOWLEDGE_SWEEP_OVERDUE_MULTIPLIER:-3}"
[[ "$SWEEP_OVERDUE_MULTIPLIER" =~ ^[0-9]+$ ]] || SWEEP_OVERDUE_MULTIPLIER=3
SYNC_OVERDUE_MULTIPLIER="${HEALTH_KNOWLEDGE_SYNC_OVERDUE_MULTIPLIER:-3}"
[[ "$SYNC_OVERDUE_MULTIPLIER" =~ ^[0-9]+$ ]] || SYNC_OVERDUE_MULTIPLIER=3
GRACE_HOURS="${HEALTH_KNOWLEDGE_STARTUP_GRACE_HOURS:-24}"
[[ "$GRACE_HOURS" =~ ^[0-9]+$ ]] || GRACE_HOURS=24
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

# Read a KEY=VALUE from a tenant env file without exposing it in logs.
env_value() {  # <env-file> <key> -> value (empty when absent/unreadable)
  local file="$1" key="$2"
  [[ -n "$file" && -r "$file" ]] || return 0
  sed -n "s/^${key}=//p" "$file" 2>/dev/null | tail -1
}

# Boolean env value with the daemon's truthy spellings (true/1/yes/on).
env_bool() {  # <env-file> <key> <default>
  local file="$1" key="$2" default="$3" value
  value="$(env_value "$file" "$key")"
  if [[ -z "$value" ]]; then
    printf '%s' "$default"
  elif [[ "${value,,}" == true || "$value" == "1" || "${value,,}" == yes || "${value,,}" == on ]]; then
    printf 'true'
  else
    printf 'false'
  fi
}

env_uint() {  # <env-file> <key> <default>
  local file="$1" key="$2" default="$3" value
  value="$(env_value "$file" "$key")"
  if [[ "$value" =~ ^[0-9]+$ ]] && [[ "$value" -gt 0 ]]; then
    printf '%s' "$value"
  else
    printf '%s' "$default"
  fi
}

# epoch from an ISO-8601 timestamp; empty on parse failure
iso_epoch() {
  local ts="$1"
  [[ -n "$ts" ]] || return 0
  date -d "$ts" +%s 2>/dev/null || true
}

# Inspect one tenant's knowledge state files and feature enablement.
# Sets globals: p_sync_enabled p_sweep_enabled p_sync_cadence p_sweep_cadence
# p_past_grace p_sync_present p_sync_failures p_sync_error_class
# p_sync_deferred p_sync_last_success_age p_sync_last_attempt_age
# p_sweep_present p_sweep_last_run_age p_malformed p_issues.
probe_one() {  # <tenant> <state-dir> <env-file>
  local tenant="$1" dir="$2" env_file="$3" sync_file sweep_file mtime
  p_sync_enabled=false; p_sweep_enabled=true
  p_sync_cadence=6; p_sweep_cadence=24
  p_past_grace=false
  p_sync_present=false; p_sync_failures=0; p_sync_error_class=""
  p_sync_deferred=0; p_sync_last_success_age=-1; p_sync_last_attempt_age=-1
  p_sweep_present=false; p_sweep_last_run_age=-1
  p_malformed=false
  p_issues=()

  p_sync_enabled="$(env_bool "$env_file" KNOWLEDGE_SYNC_ENABLED false)"
  p_sweep_enabled="$(env_bool "$env_file" KNOWLEDGE_SWEEP_ENABLED true)"
  p_sync_cadence="$(env_uint "$env_file" KNOWLEDGE_SYNC_CADENCE_HOURS 6)"
  p_sweep_cadence="$(env_uint "$env_file" KNOWLEDGE_SWEEP_CADENCE_HOURS 24)"

  # Startup grace (MR !1041 review finding 6): a just-enabled feature has not
  # had a cadence tick yet; "enabled but never observed" only degrades once
  # the configuration has been in place past the grace window. The env file's
  # mtime proxies "enabled since" (mt-admin rewrites it on every config
  # change). Without a readable env file the grace window is unknowable —
  # stay healthy and surface not_yet_observed rather than false-positive.
  if [[ "$GRACE_HOURS" -eq 0 ]]; then
    # Explicit zero-grace: degrade immediately when enabled and unobserved.
    p_past_grace=true
  elif [[ -r "$env_file" ]]; then
    mtime="$(stat -c %Y "$env_file" 2>/dev/null || stat -f %m "$env_file" 2>/dev/null || true)"
    if [[ "$mtime" =~ ^[0-9]+$ ]] && [[ "$mtime" -gt 0 ]] && (( now - mtime > GRACE_HOURS * 3600 )); then
      p_past_grace=true
    fi
  fi

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
      state_dir="$HEALTH_KNOWLEDGE_STATE_ROOT"
    else
      state_dir="/home/$user/lunarwing/state"
    fi
    env_file="${HEALTH_KNOWLEDGE_ENV_FILE:-/home/$user/lunarwing/env/lunarwing.env}"
    targets+=("$tenant|$state_dir|$env_file")
  done < <(jq -r '(.tenants // {}) | to_entries[] | "\(.key)\t\(.value.user // .key)"' "$registry" 2>/dev/null)
  if [[ ${#targets[@]} -eq 0 ]]; then
    printf '{"component":"knowledge","status":"healthy","timestamp":"%s","metrics":{"enabled":false,"reason":"no_tenants","discovery":"ports-registry","target_count":0,"instances":[]},"issues":[]}\n' "$TIMESTAMP"
    exit 0
  fi
else
  base="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}"
  targets+=("default|$base|${HEALTH_KNOWLEDGE_ENV_FILE:-$base/.env}")
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
  tenant="${target%%|*}"; rest="${target#*|}"
  state_dir="${rest%%|*}"; env_file="${rest#*|}"
  probe_one "$tenant" "$state_dir" "$env_file"

  # Per-instance overdue budgets from the ACTUAL configured cadence.
  sweep_overdue_secs=$(( p_sweep_cadence * 3600 * SWEEP_OVERDUE_MULTIPLIER ))
  if [[ -n "$SWEEP_OVERDUE_HOURS" ]]; then
    sweep_overdue_secs=$(( SWEEP_OVERDUE_HOURS * 3600 ))
  fi
  sync_overdue_secs=$(( p_sync_cadence * 3600 * SYNC_OVERDUE_MULTIPLIER ))

  inst_status="healthy"; inst_exit=0

  # Malformed state of an enabled feature is a monitoring gap; malformed state
  # of a disabled feature is inert (the daemon no longer writes it).
  if [[ "$p_malformed" == "true" && ( "$p_sync_enabled" == "true" || "$p_sweep_enabled" == "true" ) ]]; then
    inst_status="degraded"; inst_exit=1
  fi

  # Sync classification — only when the feature is actually enabled.
  if [[ "$p_sync_enabled" == "true" ]]; then
    if [[ "$p_sync_present" != "true" ]]; then
      if [[ "$p_past_grace" == "true" ]]; then
        inst_status="degraded"; inst_exit=1
        p_issues+=("knowledge sync enabled but never observed (state file absent past ${GRACE_HOURS}h grace)")
      fi
    elif [[ "$p_sync_failures" =~ ^[0-9]+$ ]] && [[ "$p_sync_failures" -ge "$FAIL_CRITICAL" ]]; then
      inst_status="critical"; inst_exit=2
      p_issues+=("knowledge sync failing: ${p_sync_failures} consecutive failures (last: ${p_sync_error_class:-unknown})")
    elif [[ "$p_sync_failures" =~ ^[0-9]+$ ]] && [[ "$p_sync_failures" -ge "$FAIL_DEGRADED" ]]; then
      inst_status="degraded"; inst_exit=1
      p_issues+=("knowledge sync failing: ${p_sync_failures} consecutive failures (last: ${p_sync_error_class:-unknown})")
    elif [[ "$p_sync_deferred" =~ ^[0-9]+$ ]] && [[ "$p_sync_deferred" -gt 0 ]]; then
      inst_status="degraded"; inst_exit=1
      p_issues+=("knowledge sync deferred notes present: ${p_sync_deferred}")
    elif [[ "$p_sync_last_attempt_age" =~ ^[0-9]+$ ]] && [[ "$p_sync_last_attempt_age" -gt "$sync_overdue_secs" ]]; then
      inst_status="degraded"; inst_exit=1
      p_issues+=("knowledge sync overdue: no attempt in ${p_sync_last_attempt_age}s (cadence ${p_sync_cadence}h)")
    fi
  fi

  # Sweep classification — only when the feature is actually enabled.
  if [[ "$p_sweep_enabled" == "true" ]]; then
    if [[ "$p_sweep_present" != "true" ]]; then
      if [[ "$p_past_grace" == "true" ]]; then
        inst_status="degraded"; inst_exit=1
        p_issues+=("knowledge sweep enabled but never observed (state file absent past ${GRACE_HOURS}h grace)")
      fi
    elif [[ "$p_sweep_last_run_age" =~ ^[0-9]+$ ]] && [[ "$p_sweep_last_run_age" -gt "$sweep_overdue_secs" ]]; then
      inst_status="degraded"; inst_exit=1
      p_issues+=("knowledge sweep overdue: last run ${p_sweep_last_run_age}s ago (cadence ${p_sweep_cadence}h)")
    fi
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
    --arg sync_enabled "$p_sync_enabled" \
    --arg sweep_enabled "$p_sweep_enabled" \
    --argjson sync_cadence "$p_sync_cadence" \
    --argjson sweep_cadence "$p_sweep_cadence" \
    --argjson sync_failures "$([[ "$p_sync_failures" =~ ^[0-9]+$ ]] && echo "$p_sync_failures" || echo 0)" \
    --argjson sync_deferred "$([[ "$p_sync_deferred" =~ ^[0-9]+$ ]] && echo "$p_sync_deferred" || echo 0)" \
    --argjson sync_last_success_age "$([[ "$p_sync_last_success_age" =~ ^[0-9]+$ ]] && echo "$p_sync_last_success_age" || echo -1)" \
    --argjson sync_last_attempt_age "$([[ "$p_sync_last_attempt_age" =~ ^[0-9]+$ ]] && echo "$p_sync_last_attempt_age" || echo -1)" \
    --argjson sweep_last_run_age "$([[ "$p_sweep_last_run_age" =~ ^[0-9]+$ ]] && echo "$p_sweep_last_run_age" || echo -1)" \
    --argjson issues "$iss_json" \
    '{tenant:$tenant,status:$status,
      sync_enabled:($sync_enabled=="true"),sweep_enabled:($sweep_enabled=="true"),
      sync_cadence_hours:$sync_cadence,sweep_cadence_hours:$sweep_cadence,
      sync_state_present:($sync_present=="true"),sweep_state_present:($sweep_present=="true"),
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
