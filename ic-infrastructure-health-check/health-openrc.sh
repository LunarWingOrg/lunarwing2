#!/bin/bash
# Health Check: OpenRC services
# Checks: per-unit service state, auto-discovered for multi-tenant hosts.
# Output: JSON to stdout
# Exit codes: 0=healthy, 1=degraded, 2=critical

set -euo pipefail

# Init-script directory. Overridable so the test suite can point at a fake
# /etc/init.d without touching the host.
INITD_DIR="${INITD_DIR:-/etc/init.d}"

# Per-unit status-probe timeout (seconds). A container unit's status() shells out
# to `podman inspect` as the tenant; with no bound, one wedged tenant could eat
# the orchestrator's whole component budget (a single `timeout 30` around this
# script) and blank remediation for EVERY tenant. 0 disables the wrapper.
STATUS_TIMEOUT="${HEALTH_OPENRC_STATUS_TIMEOUT:-8}"

# Services to check (override via env: SERVICES="xmpp-bridge lunarwing").
# Auto-discovers multi-tenant services from $INITD_DIR when no override is set.
discover_services() {
  local -A seen=()
  local svcs=() initscript name

  # Single-instance defaults.
  for name in lunarwing xmpp-bridge; do
    if [ -x "$INITD_DIR/$name" ] && [ -z "${seen[$name]:-}" ]; then
      seen[$name]=1; svcs+=("$name")
    fi
  done

  # Multi-tenant. The `lunarwing-*` glob already covers lunarwing-<t> AND every
  # per-service unit (lunarwing-proxy-<t>, lunarwing-pg-<t>, lunarwing-nanocode-<t>,
  # lunarwing-pebble-<t>, lunarwing-opencode-<t>, lunarwing-kimi-<t>, lunarwing-weechat-<t>,
  # lunarwing-weechat-adapter-<t>), so a separate lunarwing-proxy-* glob would only
  # double-list it. Dedup via `seen`.
  for initscript in "$INITD_DIR"/lunarwing-* "$INITD_DIR"/xmpp-bridge-*; do
    [ -x "$initscript" ] || continue
    name=$(basename "$initscript")
    case "$name" in lunarwing|xmpp-bridge) continue ;; esac   # base names already handled
    [ -n "${seen[$name]:-}" ] && continue
    seen[$name]=1; svcs+=("$name")
  done

  if [ ${#svcs[@]} -eq 0 ]; then
    svcs=("lunarwing" "xmpp-bridge")
  fi

  printf '%s\n' "${svcs[@]}"
}

if [ -n "${SERVICES:-}" ]; then
  read -r -a SERVICES_ARR <<<"$SERVICES"
else
  mapfile -t SERVICES_ARR < <(discover_services)
fi

issues=()
svc_results=()
overall_status="healthy"
overall_exit=0

svc_json() {
  local name="$1" state="$2" status="$3"
  cat <<EOF
  {
    "name": "$name",
    "state": "$state",
    "status": "$status"
  }
EOF
}

retired_nanocode_service() {
  case "$1" in
    lunarwing-nanocode-*) return 0 ;;
    *) return 1 ;;
  esac
}

# Probe one unit, bounded by STATUS_TIMEOUT when `timeout` is available.
# Sets globals STATUS_OUT (text) and STATUS_RC (the REAL exit code; 124/137 mean
# the probe was killed for exceeding the timeout).
probe_status() {
  local svc="$1"
  STATUS_RC=0
  if [ "$STATUS_TIMEOUT" != "0" ] && command -v timeout >/dev/null 2>&1; then
    STATUS_OUT=$(timeout -k 2 "$STATUS_TIMEOUT" rc-service "$svc" status 2>&1) || STATUS_RC=$?
  else
    STATUS_OUT=$(rc-service "$svc" status 2>&1) || STATUS_RC=$?
  fi
}

# Runlevel dir (OpenRC enabled-service symlinks). Overridable so the test suite
# can point at a fake runlevels tree without touching the host.
RUNLEVELS_DIR="${RUNLEVELS_DIR:-/etc/runlevels}"

# Multi-tenant registry (lunarwing-mt-admin.sh ports.json). Used for optional
# WeeChat and worker intent gates. Absent/unreadable file => gates inert (fail
# safe: no outage is masked).
TENANTS_FILE="${SELF_HEAL_TENANTS_FILE:-${LUNARWING_TENANTS_FILE:-/etc/lunarwing/ports.json}}"

# Extract the tenant name from a unit, or "" for single-instance / non-tenant
# units. Known per-service roles are stripped so every unit maps to its tenant:
#   lunarwing-acme -> acme   lunarwing-proxy-acme -> acme
#   lunarwing-vision-acme -> acme   lunarwing-voice-acme -> acme
#   lunarwing-weechat-adapter-acme -> acme   xmpp-bridge-acme -> acme
#   lunarwing-darkirc-adapter-acme -> acme   lunarwing-darkirc-acme -> acme
# (weechat-adapter MUST precede weechat in the case so the longer role wins.
# Same for darkirc-adapter vs darkirc.)
unit_tenant() {
  local svc="$1" rest
  case "$svc" in
    lunarwing|xmpp-bridge)  printf '' ;;
    xmpp-bridge-*)          printf '%s' "${svc#xmpp-bridge-}" ;;
    lunarwing-*)
      rest="${svc#lunarwing-}"
      case "$rest" in
        proxy-*)             rest="${rest#proxy-}" ;;
        pg-*)                rest="${rest#pg-}" ;;
        nanocode-*)          rest="${rest#nanocode-}" ;;
        opencode-*)          rest="${rest#opencode-}"; rest="${rest%-sup}" ;;
        kimi-*)              rest="${rest#kimi-}"; rest="${rest%-sup}" ;;
        pebble-*)            rest="${rest#pebble-}"; rest="${rest%-sup}" ;;
        vision-*)            rest="${rest#vision-}"; rest="${rest%-sup}" ;;
        voice-*)             rest="${rest#voice-}"; rest="${rest%-sup}" ;;
        weechat-adapter-*)   rest="${rest#weechat-adapter-}" ;;
        weechat-*)           rest="${rest#weechat-}" ;;
        darkirc-adapter-*)   rest="${rest#darkirc-adapter-}" ;;
        darkirc-*)           rest="${rest#darkirc-}" ;;
      esac
      printf '%s' "$rest" ;;
    *)                      printf '' ;;
  esac
}

# Has the operator STARTED this tenant? start-tenant `rc-update add`s the tenant's
# primary daemon (lunarwing-<tenant>) to a runlevel, so an enabled primary marks a
# started tenant. Single-instance / unknown units (empty tenant) count as started,
# so their down state stays critical (unchanged behaviour).
tenant_started() {
  local tenant="$1" primary rl
  [ -n "$tenant" ] || return 0
  # Fail safe: if the runlevels tree is missing/misconfigured we cannot prove a
  # tenant is NOT started — never mask a real outage, so treat it as started
  # (a down unit then reads `critical`, not `skipped`).
  [ -d "$RUNLEVELS_DIR" ] || return 0
  primary="lunarwing-$tenant"
  for rl in "$RUNLEVELS_DIR"/*/"$primary"; do
    { [ -e "$rl" ] || [ -L "$rl" ]; } && return 0
  done
  return 1
}

# Per-tenant WeeChat health-gate (RENDER_UNITS_SMALL_BUG follow-up). The
# lunarwing-weechat-<t> / lunarwing-weechat-adapter-<t> units are OPTIONAL and
# match the lunarwing-* discovery glob, so an intentionally unused weechat
# backend that cannot start would otherwise read critical and self-heal would
# restart-storm it (flap). `mt-admin disable-weechat <tenant>` records
# enable_weechat=false in the registry; down weechat units for such a tenant
# are then classified `skipped` (self-heal honours `skipped`).
# Fail safe everywhere: non-weechat unit, empty tenant, missing jq, or an
# unreadable registry all return 1 (NOT disabled => normal classification).
weechat_unit_disabled() {
  local svc="$1" tenant
  case "$svc" in
    lunarwing-weechat-*) ;;   # glob covers both backend and -adapter roles
    *) return 1 ;;
  esac
  tenant="$(unit_tenant "$svc")"
  [ -n "$tenant" ] || return 1
  [ -r "$TENANTS_FILE" ] || return 1
  command -v jq >/dev/null 2>&1 || return 1
  # NOTE: jq's `//` treats false as empty, so the explicit `== false` test is
  # required — `.enable_weechat // true` would read a disabled tenant as enabled.
  [ "$(jq -r --arg t "$tenant" 'if .tenants[$t].enable_weechat == false then "false" else "true" end' "$TENANTS_FILE" 2>/dev/null)" = "false" ]
}

# Down worker units are expected when worker selection is disabled or an OpenRC
# stop-worker/kill-worker persisted desired_running=false. Covers direct and
# babysitter (-sup) units. Missing tenant/registry/jq fails safe as managed.
worker_unit_intentionally_stopped() {
  local svc="$1" worker tenant result
  case "$svc" in
    lunarwing-pebble-*)   worker="pebble"; tenant="${svc#lunarwing-pebble-}" ;;
    lunarwing-opencode-*) worker="opencode"; tenant="${svc#lunarwing-opencode-}" ;;
    lunarwing-kimi-*)     worker="kimi"; tenant="${svc#lunarwing-kimi-}" ;;
    *) return 1 ;;
  esac
  case "$tenant" in *-sup) tenant="${tenant%-sup}" ;; esac
  [ -n "$tenant" ] || return 1
  [ -r "$TENANTS_FILE" ] || return 1
  command -v jq >/dev/null 2>&1 || return 1
  result="$(jq -r --arg t "$tenant" --arg w "$worker" '
    if .tenants[$t] == null then "unknown"
    elif .tenants[$t].workers[$w] != true then "stopped"
    elif .tenants[$t].worker_runtime[$w].desired_running == false then "stopped"
    else "running" end
  ' "$TENANTS_FILE" 2>/dev/null)" || return 1
  [ "$result" = "stopped" ]
}

for svc in "${SERVICES_ARR[@]}"; do
  # Init script must exist.
  if ! rc-service --exists "$svc" >/dev/null 2>&1; then
    if retired_nanocode_service "$svc"; then
      svc_results+=("$(svc_json "$svc" "missing" "skipped")")
      continue
    fi
    issues+=("service not found: $svc")
    svc_results+=("$(svc_json "$svc" "missing" "critical")")
    overall_status="critical"; overall_exit=2
    continue
  fi

  local_status="healthy"
  local_exit=0
  state="unknown"

  probe_status "$svc"

  # NOTE: STATUS_RC is the real exit code. Previously the script ran
  # `status_output=$(rc-service … ) || true; rc_exit=$?`, which captured `true`'s
  # exit (always 0) — so the `elif rc==0 → started` fallback fired for ANY
  # non-keyword output and the `→ stopped`/`→ degraded` branches were dead,
  # silently classifying ambiguous-but-down units as healthy (never remediated).
  if [ "$STATUS_RC" -eq 124 ] || [ "$STATUS_RC" -eq 137 ]; then
    state="timeout"
  elif echo "$STATUS_OUT" | grep -qi "started"; then
    state="started"
  elif echo "$STATUS_OUT" | grep -qi "stopped"; then
    state="stopped"
  elif echo "$STATUS_OUT" | grep -qi "crashed"; then
    state="crashed"
  elif [ "$STATUS_RC" -eq 0 ]; then
    state="started"
  else
    state="stopped"   # non-keyword output AND non-zero exit → down/ambiguous
  fi

  if retired_nanocode_service "$svc"; then
    local_status="skipped"; local_exit=0
  else
  case "$state" in
    stopped|crashed)
      if weechat_unit_disabled "$svc" || worker_unit_intentionally_stopped "$svc"; then
        # Optional WeeChat unit on a weechat-DISABLED tenant: down is the
        # expected state, as is an unselected or operator-stopped worker.
        # `skipped` keeps self-heal from overriding persisted intent.
        local_status="skipped"; local_exit=0
      elif tenant_started "$(unit_tenant "$svc")"; then
        local_status="critical"; local_exit=2
        issues+=("$svc not running: $state")
      else
        # F3 / O5: this unit's tenant was add-ed (units rendered) but never
        # start-ed (its primary daemon is in no runlevel). A down unit here is the
        # EXPECTED pre-start state, not an outage — report `skipped` so self-heal
        # does not start it (overriding operator intent) and the host report does
        # not flip to critical mid-provision. self-heal honours `skipped`.
        local_status="skipped"; local_exit=0
      fi
      ;;
    timeout)
      # Degraded for THIS unit only — the loop continues, so one wedged tenant
      # doesn't blank the whole component (GRACE then absorbs a one-off slow probe).
      local_status="degraded"; local_exit=1
      issues+=("$svc status probe timed out (>${STATUS_TIMEOUT}s)")
      ;;
    unknown)
      local_status="degraded"; local_exit=1
      issues+=("$svc in unexpected state: $state")
      ;;
  esac
  fi

  if [ "$local_exit" -gt "$overall_exit" ]; then
    overall_exit=$local_exit
    overall_status=$([ "$overall_exit" -eq 2 ] && echo critical || echo degraded)
  fi

  svc_results+=("$(svc_json "$svc" "$state" "$local_status")")
done

# ── Cron-scheduler blind spot (OpenRC analog of the systemd timer check) ─────
# The OpenRC watchdog and the MT health pipeline are driven by cron: an hourly
# hook under /etc/cron.hourly (or /etc/periodic/hourly), or an fcron/crontab
# managed block. If the hook is installed but no cron scheduler daemon is
# RUNNING, nothing will ever fire — watchdog, health pipeline, and self-heal
# all silently stop — while every per-service probe above still reads healthy.
# Report-only (the scheduler is never listed in .metrics.services[], so
# self-heal will not try to restart a pseudo-service) and degraded at worst:
# it never masks or downgrades a real per-service outage.

# Cron hook directories to inspect for an installed watchdog hook.
# Space-separated; overridable so the test suite can point at a fake tree.
CRON_DIRS="${HEALTH_OPENRC_CRON_DIRS:-/etc/cron.hourly /etc/periodic/hourly}"

# Cron scheduler daemons that can fire the hooks (superset of the installer's
# hourly trio and mt-admin's _ensure_cron_runlevel list).
SCHEDULER_CANDIDATES="fcron cronie dcron crond busybox-cron"

# Bounded generic command runner (same timeout policy as probe_status).
probe_cmd() {
  if [ "$STATUS_TIMEOUT" != "0" ] && command -v timeout >/dev/null 2>&1; then
    timeout -k 2 "$STATUS_TIMEOUT" "$@"
  else
    "$@"
  fi
}

# Is a LunarWing cron hook installed? Only then is a dead scheduler a problem;
# an operator who never installed the cron-driven watchdog/pipeline has nothing
# to fire, and the check stays inert (fail safe, no false degraded).
watchdog_cron_hook_installed() {
  local dir
  for dir in $CRON_DIRS; do
    [ -x "$dir/lunarwing-watchdog" ] && return 0
  done
  # fcron root tab (single-node watchdog managed block) and fcron/crontab
  # managed block (MT health pipeline). Bounded so a wedged cron config
  # cannot stall the component.
  if command -v fcrontab >/dev/null 2>&1; then
    probe_cmd fcrontab -l 2>/dev/null | grep -q "lunarwing" && return 0
  fi
  if command -v crontab >/dev/null 2>&1; then
    probe_cmd crontab -l 2>/dev/null | grep -q "lunarwing" && return 0
  fi
  return 1
}

# Sets globals SCHED_STATUS (absent|healthy|degraded) and SCHED_RUNNING.
check_scheduler() {
  SCHED_RUNNING=""
  if ! watchdog_cron_hook_installed; then
    SCHED_STATUS="absent"
    return 0
  fi
  local cron
  for cron in $SCHEDULER_CANDIDATES; do
    [ -x "$INITD_DIR/$cron" ] || continue
    probe_status "$cron"
    if [ "$STATUS_RC" -eq 0 ] && echo "$STATUS_OUT" | grep -qi "started"; then
      SCHED_RUNNING="$cron"
      break
    fi
  done
  if [ -n "$SCHED_RUNNING" ]; then
    SCHED_STATUS="healthy"
  else
    SCHED_STATUS="degraded"
    issues+=("cron hook installed but no cron scheduler is running ($SCHEDULER_CANDIDATES); watchdog and health schedules will never fire")
  fi
}

check_scheduler

# A dead scheduler degrades the component at worst — it can raise a healthy
# verdict to degraded, but never overrides a worse per-service verdict.
if [ "$SCHED_STATUS" = "degraded" ] && [ "$overall_exit" -lt 1 ]; then
  overall_exit=1
  overall_status="degraded"
fi

sched_hook=false
if [ "$SCHED_STATUS" != "absent" ]; then
  sched_hook=true
fi
sched_running=null
if [ -n "$SCHED_RUNNING" ]; then
  sched_running="\"$SCHED_RUNNING\""
fi

issues_json="[]"
if [ ${#issues[@]} -gt 0 ]; then
  issues_json=$(printf '%s\n' "${issues[@]}" | jq -R . | jq -s .)
fi

svcs_json=$(printf '%s\n' "${svc_results[@]}" | jq -s .)

cat <<EOF
{
  "component": "openrc",
  "status": "$overall_status",
  "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "metrics": {
    "services": $svcs_json,
    "scheduler": {
      "hook_installed": $sched_hook,
      "running": $sched_running,
      "status": "$SCHED_STATUS"
    }
  },
  "issues": $issues_json
}
EOF

exit $overall_exit
