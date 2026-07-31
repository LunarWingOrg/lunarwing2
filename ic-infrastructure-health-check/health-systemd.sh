#!/bin/bash
# Health Check: systemd units (services/timers)
# Checks: active state, restart count, last/next timer fire, flapping
# Output: JSON to stdout
# Exit codes: 0=healthy, 1=degraded, 2=critical

set -euo pipefail

# Units to check (override via env)
UNITS_DEFAULT=(
  "lunarwing.service"
  "xmpp-bridge.service"
  "tensorzero-gateway.service"
)

# Allow override: UNITS="a.service b.timer"
if [ -n "${UNITS:-}" ]; then
  read -r -a UNITS_ARR <<<"$UNITS"
else
  UNITS_ARR=("${UNITS_DEFAULT[@]}")
fi

# Multi-tenant host? (a tenant registry exists). On MT hosts the base system-
# level units (lunarwing.service, …) are not expected — tenants run per-user
# units — so a missing base unit is skipped instead of flagged critical.
TENANTS_FILE="${SELF_HEAL_TENANTS_FILE:-${LUNARWING_TENANTS_FILE:-/etc/lunarwing/ports.json}}"
IS_MT=false; [ -r "$TENANTS_FILE" ] && IS_MT=true

# ISSUE-007: a readable registry must not silently blank monitoring. Validate
# everything tenant discovery needs BEFORE base units are skipped on MT
# grounds; any gap downgrades the result to degraded with a diagnostic issue
# instead of letting the component report healthy with zero visibility.
MT_DISCOVERY_OK=true
MT_DISCOVERY_NOTE=""
if [ "$IS_MT" = "true" ]; then
  if ! command -v jq >/dev/null 2>&1; then
    MT_DISCOVERY_OK=false; MT_DISCOVERY_NOTE="jq not installed; cannot enumerate tenant registry"
  elif ! command -v timeout >/dev/null 2>&1; then
    MT_DISCOVERY_OK=false; MT_DISCOVERY_NOTE="timeout(1) not installed; tenant bus probes would be unbounded"
  elif ! command -v sudo >/dev/null 2>&1; then
    MT_DISCOVERY_OK=false; MT_DISCOVERY_NOTE="sudo not installed; cannot reach tenant user buses"
  elif ! jq -e 'type == "object"' "$TENANTS_FILE" >/dev/null 2>&1; then
    MT_DISCOVERY_OK=false; MT_DISCOVERY_NOTE="tenant registry is not valid JSON: $TENANTS_FILE"
  fi
fi

# Records a non-healthy (degraded) state with a diagnostic issue. Discovery
# failures degrade rather than critical: they mean "unknown", not "down".
mt_degrade() {
  issues+=("$1")
  if [ "$overall_exit" -lt 1 ]; then
    overall_exit=1; overall_status="degraded"
  fi
}

# jq missing on an MT host: the normal report assembly below needs jq, so
# emit a minimal degraded report by hand and exit — never a healthy blank.
if [ "$IS_MT" = "true" ] && [ "$MT_DISCOVERY_OK" != "true" ] \
  && ! command -v jq >/dev/null 2>&1; then
  printf '{\n  "component": "systemd",\n  "status": "degraded",\n  "timestamp": "%s",\n  "metrics": {"units": []},\n  "issues": ["multi-tenant discovery unavailable: %s"]\n}\n' \
    "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" "$MT_DISCOVERY_NOTE"
  exit 1
fi

issues=()
unit_results=()
overall_status="healthy"
overall_exit=0

unit_json() {
  local name="$1" active="$2" sub="$3" restarts="$4" extra="$5" status="$6"
  cat <<EOF
  {
    "name": "$name",
    "active_state": "$active",
    "sub_state": "$sub",
    "restart_count": $restarts,
    "status": "$status",
    "extra": $extra
  }
EOF
}

for unit in "${UNITS_ARR[@]}"; do
  if ! systemctl list-unit-files "$unit" >/dev/null 2>&1 && ! systemctl status "$unit" >/dev/null 2>&1; then
    # On a multi-tenant host the base system-level units don't exist (tenants run
    # per-user units) — skip rather than emit a phantom critical.
    [ "$IS_MT" = "true" ] && continue
    issues+=("unit not found: $unit")
    unit_results+=("$(unit_json "$unit" "missing" "missing" 0 "{}" "critical")")
    overall_status="critical"; overall_exit=2
    continue
  fi

  active=$(systemctl show -p ActiveState --value "$unit" 2>/dev/null || echo "unknown")
  sub=$(systemctl show -p SubState --value "$unit" 2>/dev/null || echo "unknown")
  restarts=$(systemctl show -p NRestarts --value "$unit" 2>/dev/null || echo 0)
  restarts=${restarts:-0}

  extra="{}"
  status="healthy"
  exit_code=0

  if [[ "$unit" == *.timer ]]; then
    last=$(systemctl show -p LastTriggerUSec --value "$unit" 2>/dev/null || echo "")
    next=$(systemctl show -p NextElapseUSecRealtime --value "$unit" 2>/dev/null || echo "")
    extra=$(jq -n --arg last "$last" --arg next "$next" '{last_trigger:$last,next_elapse:$next}')
  else
    since=$(systemctl show -p ActiveEnterTimestamp --value "$unit" 2>/dev/null || echo "")
    extra=$(jq -n --arg since "$since" '{active_since:$since}')
  fi

  if [ "$active" != "active" ]; then
    status="critical"; exit_code=2
    issues+=("$unit not active: $active/$sub")
  elif [ "$restarts" -ge 3 ]; then
    status="degraded"; exit_code=1
    issues+=("$unit restart count elevated: $restarts")
  fi

  if [ $exit_code -gt $overall_exit ]; then
    overall_exit=$exit_code
    overall_status=$([ $overall_exit -eq 2 ] && echo critical || echo degraded)
  fi

  unit_results+=("$(unit_json "$unit" "$active" "$sub" "$restarts" "$extra" "$status")")
done

# ── Per-tenant systemd USER units (multi-tenant) ────────────────────────────
# When a tenant registry exists, also probe each tenant's systemd *user* units
# (lunarwing-<t>, xmpp-bridge-<t>) on the tenant user's bus, and surface them so
# self-heal can remediate them. No-op on single-instance hosts (no registry
# file) — existing behavior is unchanged. Requires root to reach other users'
# --user buses; any unit we cannot positively load is skipped (no false
# criticals). Parallels the tenant discovery in health-openrc.sh.
_tenants_seen=0
_tenant_units_loaded=0
_tenant_probe_failures=0
_sweep_incomplete=false
if [ "$IS_MT" = "true" ] && [ "$MT_DISCOVERY_OK" = "true" ]; then
  _tenant_uctl() {  # <user> <uid> <args...> -> systemctl --user output (failure-safe, bounded)
    local u="$1" uid="$2"; shift 2
    timeout -k 2 5 sudo -n -u "$u" env XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user "$@" 2>/dev/null || true
  }
  _tenant_ctr_health() {  # <user> <uid> <container> -> health status (healthy/unhealthy/starting/"")
    local u="$1" uid="$2" ctr="$3"
    timeout -k 2 5 sudo -n -u "$u" env HOME="/home/$u" XDG_RUNTIME_DIR="/run/user/$uid" \
      podman inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$ctr" 2>/dev/null || true
  }
  # Overall wall-clock budget for the per-tenant sweep, comfortably under the
  # outer `timeout 30` in infrastructure-health-check.sh: a few dead tenant buses
  # (each bounded per-probe) must not consume the whole window and blank the
  # component. Past the deadline we stop probing and emit a partial report.
  _deadline=24
  while IFS=$'\t' read -r tname tuser tweechat; do
    [ "$SECONDS" -lt "$_deadline" ] || { _sweep_incomplete=true; break; }
    [ -n "$tname" ] && [ -n "$tuser" ] || continue
    _tenants_seen=$((_tenants_seen+1))
    tuid=$(id -u "$tuser" 2>/dev/null || echo "")
    [ -n "$tuid" ] || continue
    # Is this tenant "started"? Its primary daemon being active means every other
    # unit should be up too — so a down unit is a real outage (incl. a stopped
    # `generated` Quadlet pg/worker), not a tenant that was add-ed but not yet
    # start-ed (F3-A). One bounded show per tenant.
    tdaemon_active=false
    [ "$(_tenant_uctl "$tuser" "$tuid" show --value -p ActiveState "lunarwing-$tname.service")" = "active" ] && tdaemon_active=true
    # Enumerate ALL of this tenant's lunarwing-*/xmpp-bridge-*/weechat-* user
    # units — the systemd analog of health-openrc.sh's /etc/init.d/lunarwing-*
    # glob — instead of a hardcoded pair. Covers pg, the workers, proxy,
    # weechat(-adapter), and the daemon. Generated Quadlet units (pg/workers)
    # appear after daemon-reload. (weechat-* also catches the pre-Fold-in-A bare
    # `weechat-<t>` name; post-rename `lunarwing-*` covers it.)
    tunits=$(_tenant_uctl "$tuser" "$tuid" list-unit-files --no-legend 'lunarwing-*' 'xmpp-bridge-*' 'weechat-*' \
             | awk '$1 ~ /\.service$/ {print $1}' | sort -u)
    [ -n "$tunits" ] || tunits="lunarwing-$tname.service xmpp-bridge-$tname.service"
    for tunit in $tunits; do
      [ "$SECONDS" -lt "$_deadline" ] || { _sweep_incomplete=true; break; }
      # One show call per unit (LoadState/ActiveState/SubState/NRestarts, in
      # systemd's canonical order for a .service) instead of four round-trips —
      # leaner and bounds hang exposure on a slow tenant bus.
      mapfile -t _uf < <(_tenant_uctl "$tuser" "$tuid" show --value \
        -p LoadState -p ActiveState -p SubState -p NRestarts "$tunit")
      if [ "${_uf[0]:-}" != "loaded" ]; then
        # Distinguish a genuinely absent unit (LoadState not-found etc.) from
        # a FAILED probe: zero output means sudo/bus/timeout broke, which must
        # surface at the end instead of silently dropping the unit (ISSUE-007).
        [ "${#_uf[@]}" -eq 0 ] && _tenant_probe_failures=$((_tenant_probe_failures+1))
        continue
      fi
      _tenant_units_loaded=$((_tenant_units_loaded+1))
      uactive=${_uf[1]:-unknown}; usub=${_uf[2]:-unknown}; urestarts=${_uf[3]:-0}
      [[ "$urestarts" =~ ^[0-9]+$ ]] || urestarts=0
      ustatus="healthy"; uexit=0
      # Per-tenant WeeChat health-gate (RENDER_UNITS_SMALL_BUG follow-up): on a
      # weechat-DISABLED tenant (enable_weechat=false in the registry) a
      # not-running optional weechat backend/adapter unit is the expected
      # state, not an outage — report `skipped` so the host stays healthy and
      # self-heal never restart-storms a unit that cannot come up. A RUNNING
      # weechat unit still falls through to normal classification below.
      if [ "$tweechat" = "false" ]; then
        case "$tunit" in
          lunarwing-weechat-*|weechat-*)
            case "$uactive" in
              active) ;;  # running anyway: keep normal classification
              *)
                unit_results+=("$(unit_json "$tunit" "$uactive" "$usub" "$urestarts" \
                  "$(jq -n --arg u "$tuser" '{tenant_user:$u, weechat_enabled:false}')" "skipped")")
                continue ;;
            esac ;;
        esac
      fi
      case "$uactive/$usub" in
        active/*)
          if [ "$urestarts" -ge 3 ]; then
            ustatus="degraded"; uexit=1; issues+=("$tunit ($tuser) restart count elevated: $urestarts")
          fi
          ;;
        failed/*|*/auto-restart)
          # Ran and broke (crashed / crash-looping) — a fault regardless of enable state.
          ustatus="critical"; uexit=2; issues+=("$tunit ($tuser) not healthy: $uactive/$usub")
          ;;
        *)
          # inactive/dead etc. Distinguish a genuine outage from an intentionally-
          # not-running unit. A down unit is a real fault when the tenant is started
          # (primary daemon active — covers a stopped `generated` Quadlet pg/worker,
          # F3-A). Otherwise probe enable-state LAZILY (only here, not per-unit — F3-B
          # avoids doubling round-trips and the failure-bias of an unconditional
          # probe): an enabled or inconclusive (empty/unknown) enable-state fails
          # LOUD as critical (never mask); only a not-started tenant's
          # disabled/static/generated unit is the expected `skipped` case.
          if [ "$tdaemon_active" = "true" ]; then
            ustatus="critical"; uexit=2; issues+=("$tunit ($tuser) down while tenant running: $uactive/$usub")
          else
            case "$(_tenant_uctl "$tuser" "$tuid" show --value -p UnitFileState "$tunit")" in
              disabled|static|masked|linked|linked-runtime|generated|transient|indirect)
                ustatus="skipped"; uexit=0 ;;
              *)
                ustatus="critical"; uexit=2; issues+=("$tunit ($tuser) not active: $uactive/$usub") ;;
            esac
          fi
          ;;
      esac
      # Container units: `active` only means the container is running. Probe
      # podman health so a Running-but-wedged Postgres/worker (is-active=active
      # but not serving) is caught instead of reported healthy.
      uhealth=""
      case "$tunit" in
        lunarwing-pg-*|lunarwing-nanocode-*|lunarwing-opencode-*|lunarwing-kimi-*|lunarwing-pebble-*)
          uhealth=$(_tenant_ctr_health "$tuser" "$tuid" "${tunit%.service}")
          if [ "$uhealth" = "unhealthy" ] && [ "$uexit" -lt 2 ]; then
            ustatus="critical"; uexit=2; issues+=("$tunit ($tuser) container unhealthy")
          fi
          ;;
      esac
      if [ $uexit -gt $overall_exit ]; then
        overall_exit=$uexit
        overall_status=$([ $overall_exit -eq 2 ] && echo critical || echo degraded)
      fi
      unit_results+=("$(unit_json "$tunit" "$uactive" "$usub" "$urestarts" \
        "$(jq -n --arg u "$tuser" --arg h "$uhealth" '{tenant_user:$u} + (if $h=="" then {} else {container_health:$h} end)')" "$ustatus")")
    done
  # enable_weechat: explicit ==false test (jq `//` treats false as empty).
  done < <(jq -r '.tenants // {} | to_entries[] | "\(.key)\t\(.value.user)\t\(if .value.enable_weechat == false then "false" else "true" end)"' "$TENANTS_FILE" 2>/dev/null || true)
fi

# ISSUE-007 final invariants: an MT host must never report healthy when
# tenant discovery was unavailable, incomplete, or observed nothing —
# monitoring failure must be distinguishable from service health.
if [ "$IS_MT" = "true" ]; then
  if [ "$MT_DISCOVERY_OK" != "true" ]; then
    mt_degrade "multi-tenant discovery unavailable: $MT_DISCOVERY_NOTE"
  else
    [ "$_sweep_incomplete" = "true" ] && \
      mt_degrade "tenant sweep incomplete: deadline hit; results partial"
    if [ "$_tenants_seen" -eq 0 ]; then
      mt_degrade "tenant registry valid but lists no tenants; no units observed on MT host"
    elif [ "$_tenant_units_loaded" -eq 0 ]; then
      mt_degrade "no tenant units observable on any of $_tenants_seen tenant bus(es); discovery or bus access broken"
    fi
    [ "$_tenant_probe_failures" -gt 0 ] && \
      mt_degrade "$_tenant_probe_failures tenant unit probe(s) failed (sudo/bus/timeout); results partial"
  fi
fi

issues_json="[]"
if [ ${#issues[@]} -gt 0 ]; then
  issues_json=$(printf '%s\n' "${issues[@]}" | jq -R . | jq -s .)
fi

units_json=$(printf '%s\n' "${unit_results[@]}" | jq -s .)

cat <<EOF
{
  "component": "systemd",
  "status": "$overall_status",
  "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "metrics": {
    "units": $units_json
  },
  "issues": $issues_json
}
EOF

exit $overall_exit
