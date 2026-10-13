#!/usr/bin/env bash
#
# enable-health-fleet.sh — turn on the host-global health-check + self-heal
# pipeline for an EXISTING multi-tenant fleet, WITHOUT adding a tenant.
#
# Background: in v1.1.4 the self-heal pipeline is enabled as a side effect of
# `add-tenant`/`add-tenants` only — there is no standalone "enable" verb (see
# docs/proposals/MT-1.1.0-TO-1.1.4-UPGRADE.md §7). After an in-place upgrade you
# usually do NOT want to re-add tenants, so this wrapper invokes mt-admin's own
# `ensure_health_pipeline` directly (by sourcing mt-admin — its `main` dispatch is
# guarded with `[[ "${BASH_SOURCE[0]}" == "${0}" ]]`, so sourcing only loads the
# functions/config). No logic is duplicated, so it can't drift from mt-admin.
#
# It also adds the safety the implicit add-tenant path lacks: it refuses to enable
# the host-wide remediation schedule while any tenant daemon is DOWN
# (otherwise self-heal would immediately try to restart a tenant you stopped for
# maintenance). Run it only after the whole fleet is upgraded and back up.
#
# Run as root:
#   sudo ic/scripts/enable-health-fleet.sh [options]
#     --gotify-url <url>     Gotify base URL for escalation pages (only baked into
#     --gotify-token <tok>   health.env if it does not already exist)
#     --gotify-token-file <p> read the escalation token from file <p> (preferred;
#                             avoids exposing it on argv / in /proc/<pid>/cmdline)
#     --allow-down           proceed even if some tenant daemons are down
#     --dry-run              show what would happen; make no changes
#     --yes | -y             skip the confirmation prompt
#
# The Gotify TOKEN is a secret, so it is NOT accepted on the command line (argv is
# world-readable via /proc/<pid>/cmdline). Supply it via --gotify-token-file, or
# via the LUNARWING_MT_GOTIFY_TOKEN env var, or edit /etc/lunarwing/health.env after.
set -euo pipefail

GOTIFY_URL=""
GOTIFY_TOKEN="${LUNARWING_MT_GOTIFY_TOKEN:-}"
GOTIFY_TOKEN_FILE=""
ALLOW_DOWN=false
DRY_RUN=false
AUTO_YES=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --gotify-url)        GOTIFY_URL="$2"; shift 2 ;;
    --gotify-token-file) GOTIFY_TOKEN_FILE="$2"; shift 2 ;;
    --gotify-token)      printf 'refusing --gotify-token on argv (leaks via /proc/<pid>/cmdline); use --gotify-token-file or LUNARWING_MT_GOTIFY_TOKEN\n' >&2; exit 2 ;;
    --allow-down)        ALLOW_DOWN=true; shift ;;
    --dry-run)           DRY_RUN=true; shift ;;
    --yes|-y)            AUTO_YES=true; shift ;;
    *)                   printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
done
if [[ -n "$GOTIFY_TOKEN_FILE" ]]; then
  [[ -r "$GOTIFY_TOKEN_FILE" ]] || { printf 'token file not readable: %s\n' "$GOTIFY_TOKEN_FILE" >&2; exit 2; }
  GOTIFY_TOKEN="$(< "$GOTIFY_TOKEN_FILE")"; GOTIFY_TOKEN="${GOTIFY_TOKEN//[$'\r\n']/}"
fi

# minimal helpers (mt-admin defines say/die too; we source it below and reuse them)
_say()    { printf '%s\n' "$*"; }
_die()    { printf 'error: %s\n' "$*" >&2; exit 1; }
_banner() { printf '\n========== %s ==========\n' "$*"; }
confirm() { $AUTO_YES && return 0; local a; read -r -p "$1 [y/N] " a; [[ "$a" == y || "$a" == Y ]]; }

[[ "$(id -u)" -eq 0 ]] || _die "run as root (sudo)"
command -v jq >/dev/null 2>&1 || _die "jq required"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT="$SCRIPT_DIR/lunarwing-mt-admin.sh"
[[ -r "$MT" ]] || _die "mt-admin not found at $MT"
# The function lives in the mt-admin-lib module the dispatcher sources, not in
# the dispatcher itself (see the module map at the top of $MT): searching only
# $MT fails on every current tree and made this installer unrunnable.
{ grep -q 'ensure_health_pipeline()' "$MT" \
    || grep -rq 'ensure_health_pipeline()' "$SCRIPT_DIR/mt-admin-lib/"; } \
  || _die "mt-admin at $MT has no ensure_health_pipeline (too old?)"

# Seed Gotify creds via the env vars mt-admin reads (only used when writing a fresh
# health.env; an existing file is preserved). Export BEFORE sourcing.
[[ -n "$GOTIFY_URL"   ]] && export LUNARWING_MT_GOTIFY_URL="$GOTIFY_URL"
[[ -n "$GOTIFY_TOKEN" ]] && export LUNARWING_MT_GOTIFY_TOKEN="$GOTIFY_TOKEN"

# shellcheck source=/dev/null
source "$MT"     # loads config + functions; does NOT run main (guarded)

ensure_init_system
case "$INIT_SYSTEM" in
  systemd|openrc) : ;;
  *) _die "health pipeline is not wired for INIT_SYSTEM=$INIT_SYSTEM" ;;
esac

# WI#376 EQ-3: never arm the root health cron inside the proxy migration
# window — the cron drives `rc-service ... status` as root, which is exactly
# what the window must prevent. This script sources mt-admin directly, so
# main()'s openrc_dispatch_preflight never runs; check explicitly here. The
# DOWN-probe below hides the refusal behind >/dev/null 2>&1, which made every
# tenant merely look DOWN while --allow-down still armed the cron.
[[ "$INIT_SYSTEM" != openrc ]] || require_safe_openrc_proxy_units \
  || _die "unsafe legacy proxy units installed; run first: ic/scripts/lunarwing-mt-admin.sh render-units --all-proxies"

[[ -f "$PORTS_REGISTRY" ]] || _die "ports registry not found: $PORTS_REGISTRY"
mapfile -t TENANTS < <(jq -r '.tenants | keys[]' "$PORTS_REGISTRY" 2>/dev/null || true)
[[ ${#TENANTS[@]} -gt 0 ]] || _die "no tenants in $PORTS_REGISTRY — nothing to monitor"

_banner "Enable host-global health/self-heal pipeline ($INIT_SYSTEM)"
_say "Tenants discovered: ${TENANTS[*]}"
$DRY_RUN && _say "*** DRY RUN — no changes will be made ***"

# --- Which tenant daemons are down? (read-only; reported in all modes) --------
DOWN=()
for t in "${TENANTS[@]}"; do
  [[ -n "$t" ]] || continue
  active=false
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$t" is-active --quiet "lunarwing-${t}.service" 2>/dev/null && active=true
  else
    # Guarded like every mt-admin OpenRC call: a legacy proxy unit (WI#376)
    # refuses instead of probing, and the tenant then counts as down (safer
    # than re-arming self-heal during the migration window).
    _openrc_service "lunarwing-${t}" status >/dev/null 2>&1 && active=true
  fi
  $active || DOWN+=("$t")
done

# --- Dry-run: print the plan and exit BEFORE any gate (_die / confirm) --------
if $DRY_RUN; then
  _banner "[dry-run] would run: ensure_health_pipeline"
  [[ ${#DOWN[@]} -gt 0 ]] && _say "  (note: tenant daemons currently down: ${DOWN[*]} — a real run would refuse without --allow-down)"
  _say "  - sync $HEALTH_SRC_DIR -> $HEALTH_LIB_DIR"
  _say "  - write $HEALTH_ENV_FILE (mode 0600) if absent"
  _say "  - install launcher $HEALTH_LAUNCHER"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _say "  - enable --now lunarwing-mt-health.timer (every ${HEALTH_INTERVAL_MIN} min)"
  else
    _say "  - install fcron/cron schedule (every ${HEALTH_INTERVAL_MIN} min)"
  fi
  exit 0
fi

# --- Safety: don't arm self-heal while a tenant daemon is down ----------------
if [[ ${#DOWN[@]} -gt 0 ]]; then
  _say ""
  _say "WARNING: these tenant daemons are NOT active: ${DOWN[*]}"
  _say "Enabling self-heal now would try to restart them within ~${HEALTH_INTERVAL_MIN} min."
  if ! $ALLOW_DOWN; then
    _die "refusing to enable while tenants are down. Bring them up, or pass --allow-down if this is intended."
  fi
  _say "(--allow-down given; proceeding anyway)"
fi

# --- Gotify note --------------------------------------------------------------
if [[ -f "$HEALTH_ENV_FILE" ]]; then
  _say ""
  _say "note: $HEALTH_ENV_FILE already exists and will be PRESERVED."
  if [[ -n "$GOTIFY_URL$GOTIFY_TOKEN" ]]; then
    _say "      supplied creds will NOT overwrite it; edit GOTIFY_URL/GOTIFY_TOKEN in that file by hand."
  fi
elif [[ -z "$GOTIFY_URL$GOTIFY_TOKEN" ]]; then
  _say ""
  _say "note: no Gotify URL/token supplied — escalations will be dropped silently."
  _say "      Pass --gotify-url / --gotify-token-file (or set LUNARWING_MT_GOTIFY_TOKEN), or edit $HEALTH_ENV_FILE after."
fi

_say ""
confirm "Install + schedule the host-global self-heal pipeline (remediates ALL tenant units every ${HEALTH_INTERVAL_MIN} min)?" \
  || _die "aborted by user"

_banner "Installing"
ensure_health_pipeline

_banner "Verify"
if [[ "$INIT_SYSTEM" == "systemd" ]]; then
  if systemctl is-enabled lunarwing-mt-health.timer >/dev/null 2>&1; then
    _say "lunarwing-mt-health.timer: $(systemctl is-active lunarwing-mt-health.timer 2>/dev/null || echo unknown) / enabled"
    systemctl status lunarwing-mt-health.timer --no-pager 2>/dev/null | sed -n '1,4p' || true
  else
    _say "WARNING: lunarwing-mt-health.timer is not enabled — check the output above"
  fi
else
  _say "Check the cron schedule: (f)crontab -l | grep lunarwing-mt-health"
fi
_say ""
_say "Done. The pipeline pages only on self-heal ESCALATION (HEALTHCHECK_NOTIFY=false)."
_say "Config: $HEALTH_ENV_FILE   ·   to disable: remove the timer/cron + $HEALTH_LAUNCHER"
