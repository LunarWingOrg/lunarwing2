#!/usr/bin/env bash
# LunarWing Self-Healing Infrastructure Watchdog
#
# Reads infrastructure health-check JSON reports and attempts auto-remediation
# by restarting unhealthy services. Applies a grace period before the first
# restart, exponential backoff with jitter (linear toggle available), a
# flapping guard, post-restart verification via the component health checks,
# multi-tenant remediation, state auto-prune, per-service escalation cooldown,
# and escalation via notification.
#
# Usage:
#   lunarwing-self-heal.sh [--report <path>] [--dry-run] [--max-retries N]
#       [--backoff N] [--backoff-base N] [--backoff-max N]
#       [--backoff-strategy linear|exponential] [--grace-checks N]
#       [--prune-ttl SECONDS] [--verify-health true|false]
#
# All thresholds are also configurable via SELF_HEAL_* env vars (see below).
# Supports systemd (incl. per-tenant user units), OpenRC, and launchd.
# Designed to be called from cron-wrapper.sh after infrastructure-health-check.sh.

set -euo pipefail

VERSION="2.1.0"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Configuration ────────────────────────────────────────────────────────────

REPORT_DIR="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}/workspace/reports/health"
SELF_HEAL_STATE_DIR="${SELF_HEAL_STATE_DIR:-$REPORT_DIR/../self-heal}"
SELF_HEAL_LOG="${SELF_HEAL_LOG:-$SELF_HEAL_STATE_DIR/actions.log}"
MAX_RETRIES="${SELF_HEAL_MAX_RETRIES:-3}"

# Fixed in-run settle wait after a restart, before verifying. Kept short and
# un-jittered (a 0s settle would verify before the service has come up).
BACKOFF_SECONDS="${SELF_HEAL_BACKOFF_SECONDS:-5}"

# Exponential backoff + jitter (compute_backoff()) BETWEEN remediation attempts.
# This spaces retries ACROSS cron ticks via next_attempt_at — it is not an
# in-run sleep, so the run never blocks on a long backoff. "linear" returns
# BACKOFF_BASE unchanged (no jitter) as a backward-compatible kill switch.
BACKOFF_BASE="${SELF_HEAL_BACKOFF_BASE:-60}"
BACKOFF_MAX="${SELF_HEAL_BACKOFF_MAX:-3600}"
BACKOFF_STRATEGY="${SELF_HEAL_BACKOFF_STRATEGY:-exponential}"

# Grace period: consecutive unhealthy checks required before the first restart.
GRACE_CHECKS="${SELF_HEAL_GRACE_CHECKS:-2}"

# Flapping guard: too many restarts within the window → escalate, stop looping.
FLAP_MAX_RESTARTS="${SELF_HEAL_FLAP_MAX_RESTARTS:-5}"
FLAP_WINDOW_SECS="${SELF_HEAL_FLAP_WINDOW_SECS:-3600}"

# Restart history cap: trim restart_history[] to last N entries to prevent
# unbounded state growth. Must be ≥ FLAP_MAX_RESTARTS to preserve flap detection.
HISTORY_MAX="${SELF_HEAL_HISTORY_MAX:-20}"

# Escalation notifier timeout (seconds). Prevents a hung Gotify/notify endpoint
# from stalling the entire self-heal tick. Falls back to no timeout if GNU
# coreutils `timeout` is unavailable (macOS/launchd hosts).
ESCALATE_TIMEOUT="${SELF_HEAL_ESCALATE_TIMEOUT:-30}"

# Per-service page cooldown. This state lives outside state.json so a
# flap→recover→reflap cycle cannot erase it. Invalid values fail safely to the
# default; 0/false/no/off disables the cooldown.
ESCALATE_COOLDOWN="${SELF_HEAL_ESCALATE_COOLDOWN:-3600}"
case "$ESCALATE_COOLDOWN" in
    0|false|no|off) ESCALATE_COOLDOWN=0 ;;
    *[!0-9]*|'')
        printf 'WARNING: invalid SELF_HEAL_ESCALATE_COOLDOWN=%q; using 3600\n' "$ESCALATE_COOLDOWN" >&2
        ESCALATE_COOLDOWN=3600
        ;;
esac

# Tests and non-Gotify deployments can supply a notifier. Exit zero means a
# custom notifier delivered the page; the bundled Gotify notifier additionally
# requires GOTIFY_TOKEN to be non-empty before delivery is attempted.
NOTIFY_SCRIPT="${SELF_HEAL_NOTIFY_SCRIPT:-$SCRIPT_DIR/send-notification.sh}"

# Post-restart verification: re-run the component's health-*.sh and parse
# .status (deeper than is-active). Set false to use is-active only.
VERIFY_HEALTH="${SELF_HEAL_VERIFY_HEALTH:-true}"
HEALTH_CHECK_DIR="${SELF_HEAL_HEALTH_CHECK_DIR:-$SCRIPT_DIR}"

# Prune non-escalated state entries untouched for this long. 0 disables pruning.
STATE_PRUNE_TTL="${SELF_HEAL_STATE_PRUNE_TTL:-86400}"   # 24h

# Logical-component remediation. In multi-tenant deployments the logical
# components (gateway/xmpp/tensorzero/clickhouse) map to single-instance base
# service names (lunarwing, xmpp-bridge, ...) that DON'T exist — only per-tenant
# init units do. Set false to remediate ONLY the auto-discovered init sub-units
# and avoid phantom restarts/escalations of nonexistent base services.
REMEDY_LOGICAL="${SELF_HEAL_REMEDY_LOGICAL:-true}"

# Base services may be system units (host-global MT pipeline) or user units
# (single-node user timer). Tenant units still use their owner-specific user bus.
SYSTEMD_SCOPE="${SELF_HEAL_SYSTEMD_SCOPE:-system}"
case "$SYSTEMD_SCOPE" in
    system|user) ;;
    *)
        printf 'WARNING: invalid SELF_HEAL_SYSTEMD_SCOPE=%q; using system\n' "$SYSTEMD_SCOPE" >&2
        SYSTEMD_SCOPE=system
        ;;
esac

# NetBird lifecycle is operator-owned. Health monitoring is independent; by
# default self-heal escalates without restarting the host-global daemon. Set
# true only when operators explicitly delegate daemon restart ownership.
NETBIRD_REMEDIATE="${SELF_HEAL_NETBIRD_REMEDIATE:-false}"
[[ "$NETBIRD_REMEDIATE" == true ]] || NETBIRD_REMEDIATE=false

# Report staleness guard: refuse to act on a report older than this many seconds
# (0 = disabled). Prevents remediating on stale data if the scheduler stalls.
MAX_REPORT_AGE="${SELF_HEAL_MAX_REPORT_AGE:-0}"

# Multi-tenant registry (lunarwing-mt-admin.sh). Per-tenant systemd units are
# USER units, restarted via sudo -u <user> systemctl --user.
# Umbrel: /etc/ is non-persistent across app updates; prefer a data-volume path
# when available, falling back to /etc/ for bare-metal installs.
_default_tenants_file() {
    local candidate="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-}}/tenants/ports.json"
    [[ -f "$candidate" ]] && { printf '%s' "$candidate"; return 0; }
    printf '%s' "/etc/lunarwing/ports.json"
}
TENANTS_FILE="${SELF_HEAL_TENANTS_FILE:-$(_default_tenants_file)}"

DRY_RUN=false
REPORT_FILE=""

# ── Logging ──────────────────────────────────────────────────────────────────

timestamp() { date -u '+%Y-%m-%dT%H:%M:%SZ'; }

log_action() {
    local line
    line="$(timestamp) $*"
    mkdir -p "$(dirname "$SELF_HEAL_LOG")"
    printf '%s\n' "$line" >> "$SELF_HEAL_LOG"
    printf '%s\n' "$line" >&2
}

log() {
    printf '%s %s\n' "$(timestamp)" "$*" >&2
}

say() { printf '%s\n' "$*"; }
die() { log "FATAL: $*"; exit 1; }

# ── Arg parsing ────────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --report)            REPORT_FILE="$2"; shift 2 ;;
        --dry-run|-n)        DRY_RUN=true; shift ;;
        --max-retries)       MAX_RETRIES="$2"; shift 2 ;;
        --backoff)           BACKOFF_SECONDS="$2"; shift 2 ;;
        --backoff-base)      BACKOFF_BASE="$2"; shift 2 ;;
        --backoff-max)       BACKOFF_MAX="$2"; shift 2 ;;
        --backoff-strategy)  BACKOFF_STRATEGY="$2"; shift 2 ;;
        --grace-checks)      GRACE_CHECKS="$2"; shift 2 ;;
        --prune-ttl)
            case "${2:-}" in
                0|false|no|off) STATE_PRUNE_TTL="0"; shift 2 ;;
                *) STATE_PRUNE_TTL="$2"; shift 2 ;;
            esac
            ;;
        --verify-health)
            case "${2:-}" in
                0|false|no|off) VERIFY_HEALTH="false"; shift 2 ;;
                *) VERIFY_HEALTH="true"; shift 2 ;;
            esac
            ;;
        --help|-h)
            say "Usage: lunarwing-self-heal.sh [--report <path>] [--dry-run] [--max-retries N] [--backoff N] [--backoff-base N] [--backoff-max N] [--backoff-strategy linear|exponential] [--grace-checks N] [--prune-ttl SECONDS] [--verify-health true|false]"
            exit 0
            ;;
        *) die "unknown arg: $1 (use --help)" ;;
    esac
done

# ── Init system detection ──────────────────────────────────────────────────

detect_service_manager() {
    local override="${LUNARWING_SERVICE_MANAGER:-${IRONCLAW_SERVICE_MANAGER:-}}"
    if [[ -n "$override" ]]; then
        printf '%s' "$override"
        return 0
    fi

    [[ "$(uname -s)" == "Darwin" ]] && { printf 'launchd'; return 0; }
    [[ -e /run/openrc/softlevel ]] && { printf 'openrc'; return 0; }
    [[ -e /run/systemd/system ]]   && { printf 'systemd'; return 0; }

    command -v rc-service >/dev/null 2>&1 && ! command -v systemctl >/dev/null 2>&1 && { printf 'openrc'; return 0; }
    command -v systemctl >/dev/null 2>&1 && { printf 'systemd'; return 0; }
    command -v rc-service >/dev/null 2>&1 && { printf 'openrc'; return 0; }

    printf 'unknown'
}

SERVICE_MANAGER="$(detect_service_manager)"
log "detected service manager: $SERVICE_MANAGER"

# ── Multi-tenant helpers ────────────────────────────────────────────────────
#
# Per-tenant units are named lunarwing-<tenant>, xmpp-bridge-<tenant>,
# lunarwing-proxy-<tenant> (legacy ironclaw-proxy-<tenant> units are still
# matched). On systemd they are USER units owned by the tenant
# OS user; on OpenRC they are system services. We only treat a unit as
# per-tenant when its tenant resolves to a real user in the registry, so base
# units (lunarwing, xmpp-bridge, lunarwing-watchdog, ...) fall through safely.

unit_tenant() {
    local u="${1%.service}"
    # Strip the service-type infix to recover the tenant name. Specific prefixes
    # MUST precede the generic `lunarwing-*` (first match wins), or e.g.
    # lunarwing-pg-<t> would resolve to the bogus tenant "pg-<t>" and self-heal
    # would target the wrong (or a non-existent) unit. weechat-adapter before
    # weechat; bare weechat-* covers the pre-rename name.
    case "$u" in
        lunarwing-proxy-*)           printf '%s' "${u#lunarwing-proxy-}" ;;
        ironclaw-proxy-*)            printf '%s' "${u#ironclaw-proxy-}" ;;
        lunarwing-pg-*)              printf '%s' "${u#lunarwing-pg-}" ;;
        lunarwing-nanocode-*)        printf '%s' "${u#lunarwing-nanocode-}" ;;
        lunarwing-opencode-*)
            u="${u#lunarwing-opencode-}"
            printf '%s' "${u%-sup}" ;;
        lunarwing-opencode2-*)
            u="${u#lunarwing-opencode2-}"
            printf '%s' "${u%-sup}" ;;
        lunarwing-kimi-*)
            u="${u#lunarwing-kimi-}"
            printf '%s' "${u%-sup}" ;;
        lunarwing-pebble-*)
            u="${u#lunarwing-pebble-}"
            printf '%s' "${u%-sup}" ;;
        lunarwing-vision-*)
            u="${u#lunarwing-vision-}"
            printf '%s' "${u%-sup}" ;;

        lunarwing-voice-*)
            u="${u#lunarwing-voice-}"
            printf '%s' "${u%-sup}" ;;

        lunarwing-weechat-adapter-*) printf '%s' "${u#lunarwing-weechat-adapter-}" ;;
        lunarwing-weechat-*)         printf '%s' "${u#lunarwing-weechat-}" ;;
        xmpp-bridge-*)               printf '%s' "${u#xmpp-bridge-}" ;;
        weechat-*)                   printf '%s' "${u#weechat-}" ;;
        lunarwing-darkirc-adapter-*) printf '%s' "${u#lunarwing-darkirc-adapter-}" ;;
        lunarwing-darkirc-*)         printf '%s' "${u#lunarwing-darkirc-}" ;;
        lunarwing-*)                 printf '%s' "${u#lunarwing-}" ;;
        *)                           printf '' ;;
    esac
}

tenant_user() {
    local tenant="$1"
    [[ -n "$tenant" && -f "$TENANTS_FILE" ]] || return 0
    jq -r --arg t "$tenant" '.tenants[$t].user // empty' "$TENANTS_FILE" 2>/dev/null || true
}

_resolve_systemd_tenant_user() {
    local svc="$1" tenant user
    tenant="$(unit_tenant "$svc")"
    [[ -n "$tenant" ]] || { printf ''; return 0; }
    user="$(tenant_user "$tenant")"
    printf '%s' "$user"
}

# ── Service restart helpers ──────────────────────────────────────────────────

_systemd_use_user_bus() {
    local svc="$1"
    # NetBird daemon is host-global even when LunarWing base units run under a
    # single-node user manager.
    [[ "$SYSTEMD_SCOPE" == user && "$svc" != "${NETBIRD_SERVICE:-netbird}" ]]
}

_systemd_active() {
    if _systemd_use_user_bus "$1"; then
        systemctl --user is-active --quiet "$1"
    else
        systemctl is-active --quiet "$1"
    fi
}
_systemd_restart() {
    local svc="$1"
    if [[ "$DRY_RUN" == true ]]; then
        if _systemd_use_user_bus "$svc"; then
            log "[DRY-RUN] would run: systemctl --user restart $svc (after reset-failed)"
        else
            log "[DRY-RUN] would run: systemctl restart $svc (after reset-failed)"
        fi
        return 0
    fi
    # A start-limited unit (StartLimitBurst hit) sits in `failed` and refuses
    # `restart` until its failure counter is cleared. reset-failed is a no-op on
    # a healthy unit, so it is always safe to run first.
    if _systemd_use_user_bus "$svc"; then
        systemctl --user reset-failed "$svc" 2>/dev/null || true
        systemctl --user restart "$svc"
    else
        systemctl reset-failed "$svc" 2>/dev/null || true
        systemctl restart "$svc"
    fi
}

# Bound init-system probes/remediations: a wedged user manager (sudo into a
# dead session) or a hung OpenRC rc-service/supervise-daemon call must not hang
# the entire self-heal tick. Falls back to unbounded when timeout(1) is
# unavailable (matches ESCALATE_TIMEOUT policy).
# SELF_HEAL_PROBE_TIMEOUT is the generic knob; SELF_HEAL_SYSTEMD_PROBE_TIMEOUT
# is honoured as a legacy alias for it.
PROBE_TIMEOUT="${SELF_HEAL_PROBE_TIMEOUT:-${SELF_HEAL_SYSTEMD_PROBE_TIMEOUT:-15}}"
_sh_timeout() {
    if command -v timeout >/dev/null 2>&1; then
        timeout -k 2 "$PROBE_TIMEOUT" "$@"
    else
        "$@"
    fi
}

# Per-tenant systemd USER units: run as the tenant user against their bus.
_systemd_user_restart() {
    local user="$1" unit="$2" uid
    uid="$(id -u "$user" 2>/dev/null || printf '?')"
    if [[ "$DRY_RUN" == true ]]; then
        log "[DRY-RUN] would run: sudo -n -u $user env XDG_RUNTIME_DIR=/run/user/$uid systemctl --user restart $unit (after reset-failed)"
        return 0
    fi
    [[ "$uid" == '?' ]] && { log "WARNING: cannot resolve uid for tenant user $user"; return 1; }
    # `-n`: never prompt for a password in this non-interactive (timer/cron)
    # context. reset-failed clears a start-limited (failed) unit so restart is
    # honored; harmless on a healthy unit.
    _sh_timeout sudo -n -u "$user" env "XDG_RUNTIME_DIR=/run/user/$uid" systemctl --user reset-failed "$unit" 2>/dev/null || true
    _sh_timeout sudo -n -u "$user" env "XDG_RUNTIME_DIR=/run/user/$uid" systemctl --user restart "$unit"
}
_systemd_user_active() {
    local user="$1" unit="$2" uid
    uid="$(id -u "$user" 2>/dev/null || printf '?')"
    [[ "$uid" == '?' ]] && return 1
    _sh_timeout sudo -n -u "$user" env "XDG_RUNTIME_DIR=/run/user/$uid" systemctl --user is-active --quiet "$unit"
}

# OpenRC probes are bounded too: a wedged supervise-daemon or hung rc-service
# must fail the probe (bounded), not stall the whole remediation tick.
_openrc_active() { _sh_timeout rc-service "$1" status >/dev/null 2>&1; }
_openrc_restart() {
    local svc="$1"
    if [[ "$DRY_RUN" == true ]]; then
        log "[DRY-RUN] would run: rc-service $svc restart"
        return 0
    fi
    _sh_timeout rc-service "$svc" restart
}

# WI#410: runtime re-check of the WI#376 EQ-3 migration-window guard.
# ic/scripts/enable-health-fleet.sh refuses to ARM this root cron while an
# unsafe legacy proxy unit is installed, but that check is arming-time only:
# a pre-migration unit can reappear (or an interrupted render-units can leave
# one behind) after the cron is armed, and the next root tick would then drive
# rc-service against it — status probes here, and restarts through
# _openrc_restart — with no gate. main() re-checks the installed units as data
# before anything else and refuses with the same remediation. The contract is
# mt-admin's own openrc_proxy_unit_is_safe, sourced from
# openrc-proxy-unit-contract.sh beside this script (one implementation for
# the renderer, mt-admin's preflight guard, health-openrc.sh and this script;
# WI#410 ID-2 — see health-openrc.sh for the sync note). A missing library is
# itself a refusal: this script never drives rc-service as root unguarded.
# LUNARWING_MT_OPENRC_INITD_DIR is mt-admin's override name; INITD_DIR
# (health-openrc.sh's) is accepted as an alias (WI#410 ID-7).
OPENRC_INITD_DIR="${LUNARWING_MT_OPENRC_INITD_DIR:-${INITD_DIR:-/etc/init.d}}"
OPENRC_PROXY_CONTRACT="$SCRIPT_DIR/openrc-proxy-unit-contract.sh"
OPENRC_PROXY_CONTRACT_MISSING=true
if [[ -f "$OPENRC_PROXY_CONTRACT" && ! -L "$OPENRC_PROXY_CONTRACT" ]]; then
    # shellcheck source=openrc-proxy-unit-contract.sh
    source "$OPENRC_PROXY_CONTRACT"
    OPENRC_PROXY_CONTRACT_MISSING=false
fi

# WI#410 ID-1: a persisting refusal must page. HEALTHCHECK_NOTIFY is false on
# MT hosts and health-openrc.sh's critical report carries no restart target,
# so without this nothing would page while one bad unit blanks all OpenRC
# probing and remediation fleet-wide, every tick, indefinitely. Page once per
# ESCALATE_COOLDOWN through the existing escalation path (a durable ESCALATE
# line in actions.log, the cooldown stamp, the notifier), then fail closed:
# exit 1, no lock taken, state.json never written (mkdir -p only — not
# ensure_state_dir, which seeds state.json).
refuse_openrc_tick() {                                       # <message> <reason>
    log "FATAL: $1"
    mkdir -p "$SELF_HEAL_STATE_DIR"
    escalate_service "openrc-proxy-units" 0 "$2" || true
    exit 1
}

_launchd_active() {
    local label="$1"
    launchctl list 2>/dev/null | grep -q "$label"
}
_launchd_restart() {
    local label="$1"
    if [[ "$DRY_RUN" == true ]]; then
        log "[DRY-RUN] would run: launchctl stop/start $label"
        return 0
    fi
    launchctl stop "$label" 2>/dev/null || true
    sleep 1
    launchctl start "$label"
}

restart_service() {
    local svc="$1"
    # Close the self-heal lock fd (200, opened in main()) for the restart and ALL
    # its children. rc-service/systemctl spawn a long-lived supervise-daemon that
    # would otherwise INHERIT fd 200 and hold the flock forever — wedging every
    # later self-heal run with "another self-heal instance is running". The
    # `200>&-` on the case compound closes it for the whole dispatch subtree.
    case "$SERVICE_MANAGER" in
        systemd)
            local user
            user="$(_resolve_systemd_tenant_user "$svc")"
            if [[ -n "$user" ]]; then
                _systemd_user_restart "$user" "$svc"
            else
                _systemd_restart "$svc"
            fi
            ;;
        openrc)  _openrc_restart "$svc" ;;
        launchd) _launchd_restart "$svc" ;;
        *)       log "WARNING: unknown service manager, cannot restart $svc"; return 1 ;;
    esac >&2 200>&-
}

check_service_active() {
    local svc="$1"
    case "$SERVICE_MANAGER" in
        systemd)
            local user
            user="$(_resolve_systemd_tenant_user "$svc")"
            if [[ -n "$user" ]]; then
                _systemd_user_active "$user" "$svc"
            else
                _systemd_active "$svc"
            fi
            ;;
        openrc)  _openrc_active "$svc" ;;
        launchd) _launchd_active "$svc" ;;
        *)       return 1 ;;
    esac
}

# Tenant-aware SubState for a systemd unit (crash-loop detection in verify).
# Empty when unavailable (non-systemd, or a mock that doesn't model `show`).
service_substate() {
    local svc="$1" user uid
    [[ "$SERVICE_MANAGER" == systemd ]] || return 0
    user="$(_resolve_systemd_tenant_user "$svc")"
    if [[ -n "$user" ]]; then
        uid="$(id -u "$user" 2>/dev/null || echo '?')"
        [[ "$uid" == '?' ]] && return 0
        _sh_timeout sudo -n -u "$user" env "XDG_RUNTIME_DIR=/run/user/$uid" systemctl --user show -p SubState --value "$svc" 2>/dev/null || true
    else
        if _systemd_use_user_bus "$svc"; then
            systemctl --user show -p SubState --value "$svc" 2>/dev/null || true
        else
            systemctl show -p SubState --value "$svc" 2>/dev/null || true
        fi
    fi
}

# ── Component → service mapping ─────────────────────────────────────────────

declare -A SERVICE_MAP=(
    [gateway]="lunarwing"
    [xmpp]="xmpp-bridge"
    [tensorzero]="tensorzero-gateway"
    [clickhouse]="clickhouse-server"
    [lunarvision]="${LUNARVISION_SERVICE:-ocr-sidecar}"
    [lunarvoice]="${LUNARVOICE_SERVICE:-voice-sidecar}"
    [netbird]="${NETBIRD_SERVICE:-netbird}"
)

# Component → health-check script, for post-restart verification.
declare -A COMPONENT_CHECK_MAP=(
    [gateway]="health-gateway.sh"
    [xmpp]="health-xmpp.sh"
    [tensorzero]="health-tensorzero.sh"
    [clickhouse]="health-clickhouse.sh"
    [lunarvision]="health-lunarvision.sh"
    [lunarvoice]="health-lunarvoice.sh"
    [netbird]="health-netbird.sh"
)

# Components that represent features of another service, not their own service.
declare -A NO_REMEDY=(
    [omemo]=1
    [ratelimit]=1
    [models]=1
    [llm_streams]=1
)

# Components where detection is useful but automatic restart is unsafe. These
# still enter the grace/escalation path instead of disappearing as NO_REMEDY.
declare -A MONITOR_ONLY=()
MONITOR_ONLY[lunarvoice]=1
MONITOR_ONLY[runtime]=1
MONITOR_ONLY[knowledge]=1
if [[ "$NETBIRD_REMEDIATE" != true ]]; then
    MONITOR_ONLY[netbird]=1
fi

# Monitor-only diagnostic components that own NO service at all (MR !1041
# review finding 2): runtime and knowledge report per-tenant instance
# diagnostics, so their failures enter the monitor-only escalation path with
# tenant attribution (synthetic "<comp>@<tenant>" identities built in main),
# never a SERVICE_MAP restart. Without this registration, standard MT
# configuration (SELF_HEAL_REMEDY_LOGICAL=false) silently skipped their
# critical results because there was no service to map.
declare -A DIAGNOSTIC_COMPONENTS=(
    [runtime]=1
    [knowledge]=1
)

# Service identities that inherit monitor-only treatment regardless of which
# component reported them (WI#180 F6): the logical lunarvoice component is
# monitor-only, but the same underlying service reported under openrc/systemd
# used to enter normal restart remediation because policy was keyed by the
# reporting component. The LunarVoice service and its -sup supervisor must
# never be auto-restarted through ANY reporting path.
declare -A MONITOR_ONLY_SERVICE=()
MONITOR_ONLY_SERVICE["${LUNARVOICE_SERVICE:-voice-sidecar}"]=1
MONITOR_ONLY_SERVICE[lunarwing-voice]=1
MONITOR_ONLY_SERVICE[lunarwing-voice-sup]=1

# Does a service name match a monitor-only service identity? Matches the bare
# name, a tenant-scoped unit (lunarwing-voice-<t>), and the -sup supervisor.
service_is_monitor_only() {
    local svc="$1" bare
    [[ -n "${MONITOR_ONLY_SERVICE[$svc]:-}" ]] && return 0
    # Tenant-scoped init units: lunarwing-voice-<tenant>[-sup].
    case "$svc" in
        lunarwing-voice-*) return 0 ;;
    esac
    # OpenRC/systemd voice-sidecar base name (single-node): voice-sidecar[-sup].
    bare="${svc%.service}"
    case "$bare" in
        voice-sidecar|voice-sidecar-sup) return 0 ;;
    esac
    return 1
}

# ── Exponential backoff with jitter (kumogakure's compute_backoff) ───────────
#
# compute_backoff <attempt> -> integer seconds until the next attempt.
#   linear:      returns BACKOFF_BASE unchanged (no jitter, kill switch).
#   exponential: BACKOFF_BASE * 2^(attempt-1), capped at BACKOFF_MAX, then full
#                jitter: uniform random in [0, delay].
# Used to schedule next_attempt_at across runs (NOT as an in-run sleep), so a
# jittered 0 just means "retry on the next tick".

compute_backoff() {
    local attempt="${1:-1}"

    if [[ "$BACKOFF_STRATEGY" == "linear" ]]; then
        printf '%s' "${BACKOFF_BASE:-60}"
        return 0
    fi

    if ! [[ "$attempt" =~ ^[0-9]+$ ]] || [[ "$attempt" -le 0 ]]; then
        printf '0'; return 0
    fi
    # Hard cap: services escalated and idling for weeks would otherwise run a
    # huge exponentiation loop. Defensive only — practical attempts are <10.
    if [[ "$attempt" -gt 20 ]]; then
        printf '%s' "${max:-3600}"
        return 0
    fi
    local base="${BACKOFF_BASE:-60}"
    local max="${BACKOFF_MAX:-3600}"
    if ! [[ "$base" =~ ^[0-9]+$ ]] || ! [[ "$max" =~ ^[0-9]+$ ]]; then
        log "WARNING: invalid backoff config base=$base max=$max; falling back to 0"
        printf '0'; return 0
    fi

    # BASE * 2^(attempt-1), capped at MAX (cap inside the loop to avoid overflow).
    local delay="$base" i
    for ((i = 1; i < attempt; i++)); do
        delay=$((delay * 2))
        if [[ "$delay" -ge "$max" ]]; then delay="$max"; break; fi
    done
    if [[ "$delay" -gt "$max" ]]; then delay="$max"; fi

    if [[ "$delay" -le 0 ]]; then printf '0'; return 0; fi
    # $RANDOM is 15-bit (0-32767). Above that the distribution skews; use urandom.
    if [[ "$delay" -gt 32767 ]]; then
        local rand
        rand=$(od -An -tu2 -N2 /dev/urandom | tr -d ' ')
        printf '%s' "$(( rand % (delay + 1) ))"
    else
        printf '%s' "$(( RANDOM % (delay + 1) ))"
    fi   # full jitter [0, delay]
}

# ── State management ────────────────────────────────────────────────────────
#
# state.json maps service -> { retries, escalated, last_attempt, escalatedAt,
# next_attempt_at, consecutive_unhealthy, first_unhealthy_at, last_unhealthy_at,
# restart_history[] }. Helpers operate on a JSON string and echo a new one.

STATE_FILE="$SELF_HEAL_STATE_DIR/state.json"

ensure_state_dir() {
    mkdir -p "$SELF_HEAL_STATE_DIR"
    [[ "$DRY_RUN" == true || -f "$STATE_FILE" ]] || echo '{}' > "$STATE_FILE"
}

load_state() {
    if [[ ! -f "$STATE_FILE" ]]; then
        echo '{}'; return 0
    fi
    local parsed
    # Require a non-empty JSON OBJECT. `jq -r '.'` exits 0 with EMPTY output on a
    # 0-byte/whitespace file, which would silently yield empty state — so backoff,
    # the flap circuit-breaker, and max-retries escalation never accumulate across
    # ticks. The type-guard makes empty/whitespace/non-object all fail into the
    # corrupt-rename path below and recover with a fresh {} on the next tick.
    if [[ -s "$STATE_FILE" ]] \
        && parsed="$(jq -e 'if type=="object" then . else error end' "$STATE_FILE" 2>/dev/null)" \
        && [[ -n "$parsed" ]]; then
        printf '%s' "$parsed"
    else
        # State file is corrupt (truncated write, disk error, etc.). Preserve it
        # for forensics under a FIXED single-slot .corrupt name (no timestamp) so
        # it overwrites last-wins and cannot accumulate unbounded across recurring
        # corruption. Then start fresh so escalated state can be rebuilt from the
        # next tick.
        if [[ "$DRY_RUN" == true ]]; then
            log "WARNING: state.json is corrupt — [DRY-RUN] would rename to state.json.corrupt and start fresh"
        else
            mv "$STATE_FILE" "$STATE_FILE.corrupt" 2>/dev/null || true
            log "WARNING: state.json is corrupt — renamed to state.json.corrupt and starting fresh"
        fi
        echo '{}'
    fi
}

save_state() {
    local state="$1" tmp
    tmp="$(mktemp "$STATE_FILE.tmp.XXXXXX")"
    printf '%s\n' "$state" > "$tmp"
    mv "$tmp" "$STATE_FILE"
}

persist_state() {
    if [[ "$DRY_RUN" == true ]]; then
        log "[DRY-RUN] state changes not persisted"
        return 0
    fi
    save_state "$1"
}

state_get_num() {
    local state="$1" svc="$2" field="$3" default="${4:-0}"
    echo "$state" | jq -r --arg s "$svc" --arg f "$field" --argjson d "$default" '.[$s][$f] // $d'
}
state_set_num() {
    local state="$1" svc="$2" field="$3" val="$4"
    echo "$state" | jq --arg s "$svc" --arg f "$field" --argjson v "$val" \
        '.[$s] = (.[$s] // {}) | .[$s][$f] = $v'
}
state_get_bool() {
    local state="$1" svc="$2" field="$3"
    echo "$state" | jq -r --arg s "$svc" --arg f "$field" '.[$s][$f] // false'
}

get_retry_count() { state_get_num "$1" "$2" retries 0; }

set_retry_count() {
    local state="$1" svc="$2" count="$3"
    echo "$state" | jq --arg svc "$svc" --argjson count "$count" \
        '.[$svc] = (.[$svc] // {}) | .[$svc].retries = $count | .[$svc].last_attempt = (now | floor) | .[$svc].escalated = (.[$svc].escalated // false)'
}

mark_escalated() {
    local state="$1" svc="$2"
    echo "$state" | jq --arg svc "$svc" \
        '.[$svc] = (.[$svc] // {}) | .[$svc].escalated = true | .[$svc].escalatedAt = (now | floor)'
}

clear_service_state() {
    local state="$1" svc="$2" since
    since=$(( $(date +%s) - FLAP_WINDOW_SECS ))
    echo "$state" | jq --arg svc "$svc" --argjson since "$since" '
        .[$svc] as $old
        | [($old.restart_history // [])[] | select(. >= $since)] as $history
        | if ($history | length) > 0 then
            .[$svc] = {restart_history: $history, last_attempt: (now | floor), escalated: false}
          else
            del(.[$svc])
          end'
}

# Record an unhealthy observation: bump streak + first/last_unhealthy_at.
record_unhealthy() {
    local state="$1" svc="$2" now="$3"
    echo "$state" | jq --arg s "$svc" --argjson now "$now" '
        .[$s] = (.[$s] // {})
        | .[$s].consecutive_unhealthy = ((.[$s].consecutive_unhealthy // 0) + 1)
        | .[$s].first_unhealthy_at = (.[$s].first_unhealthy_at // $now)
        | .[$s].last_unhealthy_at = $now'
}

# Flapping window helpers.
flap_count() {
    local state="$1" svc="$2" since="$3"
    echo "$state" | jq -r --arg s "$svc" --argjson since "$since" \
        '[(.[$s].restart_history // [])[] | select(. >= $since)] | length'
}
state_push_restart() {
    local state="$1" svc="$2" epoch="$3" since="$4"
    echo "$state" | jq --arg s "$svc" --argjson e "$epoch" --argjson since "$since" --argjson max "$HISTORY_MAX" \
        '.[$s] = (.[$s] // {})
         | .[$s].restart_history = (
             [ ((.[$s].restart_history // [])[] | select(. >= $since)), $e ]
             | if length > $max then .[-$max:] else . end
         )'
}

# Drop entries untouched past TTL that are not escalated and not currently
# unhealthy. STATE_PRUNE_TTL=0 disables. Recency = max(last_attempt, last_unhealthy_at).
prune_state() {
    local state="$1" now="$2"; shift 2
    [[ "$STATE_PRUNE_TTL" == "0" ]] && { printf '%s' "$state"; return 0; }
    local seen_json
    if [[ $# -gt 0 ]]; then seen_json="$(printf '%s\n' "$@" | jq -R . | jq -s .)"; else seen_json='[]'; fi
    echo "$state" | jq --argjson now "$now" --argjson ttl "$STATE_PRUNE_TTL" --argjson seen "$seen_json" '
        with_entries(select(
            (.value.escalated == true)
            or (.key | IN($seen[]))
            or (([.value.last_attempt, .value.last_unhealthy_at] | map(. // 0) | max) >= ($now - $ttl))
        ))'
}

# ── Notification escalation ─────────────────────────────────────────────────

_service_hash() {
    local svc="$1" digest=""
    if command -v sha256sum >/dev/null 2>&1; then
        digest="$(printf '%s' "$svc" | sha256sum)"; digest="${digest%% *}"
    elif command -v shasum >/dev/null 2>&1; then
        digest="$(printf '%s' "$svc" | shasum -a 256)"; digest="${digest%% *}"
    elif command -v openssl >/dev/null 2>&1; then
        digest="$(printf '%s' "$svc" | openssl dgst -sha256 2>/dev/null)"; digest="${digest##* }"
    else
        return 1
    fi
    [[ "$digest" =~ ^[[:xdigit:]]{64}$ ]] || return 1
    printf '%s' "${digest,,}"
}

_service_file_key() {
    local svc="$1" safe digest
    safe="$(printf '%s' "$svc" | tr -c 'A-Za-z0-9_.-' '_' | cut -c1-48)"
    digest="$(_service_hash "$svc")" || return 1
    printf '%s.%s' "$safe" "$digest"
}

# Include a readable prefix and a strong digest. Sanitization alone is unsafe:
# distinct report-supplied names such as a/b and a?b would share one cooldown.
_cooldown_file() {
    local key
    key="$(_service_file_key "$1")" || return 1
    printf '%s/.last_escalation_page.%s' "$SELF_HEAL_STATE_DIR" "$key"
}

_send_notification() {
    local status="$1" report_path="$2" svc="${3:-}"
    local notify_script="$NOTIFY_SCRIPT" cooldown_file="" now last age

    if [[ -n "$svc" && "$ESCALATE_COOLDOWN" -gt 0 ]]; then
        if cooldown_file="$(_cooldown_file "$svc")"; then
            last="$(cat "$cooldown_file" 2>/dev/null || printf '0')"
            [[ "$last" =~ ^[0-9]+$ ]] || last=0
            now="$(date +%s)"
            age=$((now - last))
            if [[ "$last" -gt 0 && "$age" -ge 0 && "$age" -lt "$ESCALATE_COOLDOWN" ]]; then
                log "COOLDOWN: page for $svc deferred (last page ${age}s ago < ${ESCALATE_COOLDOWN}s cooldown); will retry next tick"
                log_action "ESCALATE_PAGE_SUPPRESSED target=$svc last_page_age=${age}s cooldown=${ESCALATE_COOLDOWN}s"
                return 2
            elif [[ "$last" -gt "$now" ]]; then
                log "WARNING: future cooldown timestamp for $svc ignored"
            fi
        else
            log "WARNING: no SHA-256 tool available; cooldown disabled for $svc"
        fi
    fi

    local delivered=false
    if [[ -x "$notify_script" ]]; then
        # CRITICAL: redirect the notifier's stdout to stderr. escalate_service runs
        # inside remediate_component, whose STDOUT is captured as the returned state
        # JSON (state="$(remediate_component ...)"). Any stdout here corrupts that
        # JSON, so the next prune_state jq aborts under `set -e` BEFORE save_state.
        # A non-zero notifier result leaves escalation pending for the next tick.
        #
        # Guard with timeout so a hung Gotify endpoint can't stall the self-heal tick.
        if [[ "$notify_script" == "$SCRIPT_DIR/send-notification.sh" && -z "${GOTIFY_TOKEN:-}" ]]; then
            log "WARNING: GOTIFY_TOKEN is not set; escalation page was not delivered"
        elif command -v timeout >/dev/null 2>&1; then
            if timeout "${ESCALATE_TIMEOUT}" "$notify_script" "$status" "$report_path" >&2; then
                delivered=true
            else
                log "WARNING: escalation notification failed (page may not have been delivered)"
            fi
        else
            # Fallback for macOS/launchd hosts without GNU coreutils timeout.
            if "$notify_script" "$status" "$report_path" >&2; then
                delivered=true
            else
                log "WARNING: escalation notification failed (page may not have been delivered)"
            fi
        fi
    else
        log "WARNING: send-notification.sh not found at $notify_script; cannot escalate"
    fi

    if [[ "$delivered" == true && -n "$svc" && "$ESCALATE_COOLDOWN" -gt 0 && -n "$cooldown_file" ]]; then
        local cooldown_tmp="${cooldown_file}.tmp.$$"
        if (umask 077; date +%s > "$cooldown_tmp") && mv "$cooldown_tmp" "$cooldown_file"; then
            :
        else
            rm -f "$cooldown_tmp"
            log "WARNING: could not persist escalation cooldown for $svc"
        fi
    fi
    [[ "$delivered" == true ]]
}

# escalate_service <svc> <retries> [reason]   (retries passed explicitly — no $STATE)
escalate_service() {
    local svc="$1" retries="${2:-0}" reason="${3:-max_retries}"
    log "ESCALATING: $svc ($reason, retries=$retries)"
    log_action "ESCALATE target=$svc retries=$retries reason=$reason"
    if [[ "$DRY_RUN" == true ]]; then
        log "[DRY-RUN] would send escalation notification for $svc"
        return 2
    fi

    local esc_report esc_key
    esc_key="$(_service_file_key "$svc")" || esc_key="unknown.$$"
    esc_report="$(mktemp "$SELF_HEAL_STATE_DIR/escalation-$(date +%Y%m%d%H%M%S)-${esc_key}.json.XXXXXX")"
    jq -n --arg svc "$svc" --arg now "$(timestamp)" --arg reason "$reason" --argjson retries "$retries" \
        '{escalated: true, service: $svc, timestamp: $now, retries: $retries, reason: $reason, action: "manual_intervention_required"}' \
        > "$esc_report"

    local notify_rc
    if _send_notification "critical" "$esc_report" "$svc"; then
        notify_rc=0
    else
        notify_rc=$?
    fi
    rm -f "$esc_report"
    return "$notify_rc"
}

escalate_and_update_state() {
    local state="$1" svc="$2" retries="$3" reason="$4"
    if [[ "$DRY_RUN" == true ]]; then
        escalate_service "$svc" "$retries" "$reason" || true
    elif escalate_service "$svc" "$retries" "$reason"; then
        state="$(mark_escalated "$state" "$svc")"
    else
        log "ESCALATION PENDING: page for $svc not delivered; will retry next tick"
        log_action "ESCALATE_PENDING target=$svc retries=$retries reason=$reason"
    fi
    printf '%s' "$state"
}

# ── Post-restart verification ───────────────────────────────────────────────

# verify_restart <comp> <svc> -> 0 healthy, 1 still-unhealthy
verify_restart() {
    local comp="$1" svc="$2"
    if [[ "$VERIFY_HEALTH" == true ]]; then
        local check="${COMPONENT_CHECK_MAP[$comp]:-}"
        if [[ -n "$check" && -x "$HEALTH_CHECK_DIR/$check" ]]; then
            local out status
            out="$("$HEALTH_CHECK_DIR/$check" 2>/dev/null || true)"
            status="$(echo "$out" | jq -r '.status // "unknown"' 2>/dev/null || echo unknown)"
            case "$status" in
                healthy)            log "VERIFY: $comp health check healthy after restart"; return 0 ;;
                degraded|critical)  log "VERIFY: $comp still $status after restart"; return 1 ;;
                *)                  log "VERIFY: $comp check inconclusive ($status); falling back to is-active" ;;
            esac
        fi
    fi
    # is-active fallback. A crash-looping unit (Restart=) is briefly `active`
    # between crashes, so a single is-active sample can false-positive; on systemd
    # also reject an `auto-restart`/`failed` substate (the crash-loop signature).
    check_service_active "$svc" || return 1
    local _sub; _sub="$(service_substate "$svc")"
    case "$_sub" in
        auto-restart|failed) log "VERIFY: $svc not stable after restart (substate=$_sub)"; return 1 ;;
    esac
    return 0
}

# ── Main remediation logic ──────────────────────────────────────────────────

remediate_component() {
    local comp="$1" state="$2" services="$3"
    local svc now
    now="$(date +%s)"

    for svc in $services; do
        local retries escalated old_obs obs flap_since flaps next_at delay

        retries="$(get_retry_count "$state" "$svc")"
        escalated="$(state_get_bool "$state" "$svc" escalated)"

        # Record this unhealthy observation (grace period bookkeeping).
        old_obs="$(state_get_num "$state" "$svc" consecutive_unhealthy 0)"
        obs=$((old_obs + 1))
        state="$(record_unhealthy "$state" "$svc" "$now")"

        if [[ "$escalated" == true ]]; then
            log "SKIP: $svc already escalated (manual intervention pending)"
            continue
        fi

        # Flapping guard: too many restarts within the window → escalate.
        flap_since=$(( now - FLAP_WINDOW_SECS ))
        flaps="$(flap_count "$state" "$svc" "$flap_since")"
        if [[ "$flaps" -ge "$FLAP_MAX_RESTARTS" ]]; then
            log "FLAPPING: $svc restarted $flaps times in ${FLAP_WINDOW_SECS}s; escalating instead"
            state="$(escalate_and_update_state "$state" "$svc" "$retries" "flapping")"
            continue
        fi

        # Max retries reached → escalate.
        if [[ "$retries" -ge "$MAX_RETRIES" ]]; then
            state="$(escalate_and_update_state "$state" "$svc" "$retries" "max_retries")"
            continue
        fi

        # Grace period: require N consecutive unhealthy checks first.
        if [[ "$obs" -lt "$GRACE_CHECKS" ]]; then
            log "GRACE: $svc unhealthy $obs/$GRACE_CHECKS observation(s); deferring restart"
            log_action "GRACE target=$svc obs=$obs/$GRACE_CHECKS"
            continue
        fi

        if [[ "${MONITOR_ONLY[$comp]:-}" == "1" ]] || service_is_monitor_only "$svc"; then
            state="$(escalate_and_update_state "$state" "$svc" "$retries" "monitor_only")"
            continue
        fi

        # Backoff gate: wait until next_attempt_at before retrying.
        next_at="$(state_get_num "$state" "$svc" next_attempt_at 0)"
        if [[ "$now" -lt "$next_at" ]]; then
            log "BACKOFF: $svc waiting (next attempt at epoch $next_at, $((next_at - now))s)"
            log_action "BACKOFF target=$svc until=$next_at"
            continue
        fi

        log "RESTART: $svc (component=$comp, retries=$retries, obs=$obs, flaps=$flaps)"
        log_action "RESTART_BEGIN target=$svc component=$comp retries=$retries"
        state="$(state_push_restart "$state" "$svc" "$now" "$flap_since")"

        if restart_service "$svc"; then
            sleep "$BACKOFF_SECONDS"
            if verify_restart "$comp" "$svc"; then
                log "SUCCESS: $svc healthy after restart"
                log_action "RESTART_OK target=$svc"
                state="$(clear_service_state "$state" "$svc")"
            else
                retries=$((retries + 1))
                delay="$(compute_backoff "$retries")"
                state="$(set_retry_count "$state" "$svc" "$retries")"
                state="$(state_set_num "$state" "$svc" next_attempt_at $(( now + delay )))"
                log "WARNING: $svc still unhealthy after restart (retries=$retries, next try in ~${delay}s)"
                log_action "RESTART_PARTIAL target=$svc retries=$retries backoff=${delay}s strategy=$BACKOFF_STRATEGY"
            fi
        else
            retries=$((retries + 1))
            delay="$(compute_backoff "$retries")"
            state="$(set_retry_count "$state" "$svc" "$retries")"
            state="$(state_set_num "$state" "$svc" next_attempt_at $(( now + delay )))"
            log "FAILURE: restart command failed for $svc (retries=$retries, next try in ~${delay}s)"
            log_action "RESTART_FAIL target=$svc retries=$retries backoff=${delay}s strategy=$BACKOFF_STRATEGY"
        fi
    done

    printf '%s' "$state"
}

# ── Report discovery ────────────────────────────────────────────────────────

find_latest_report() {
    local latest
    # ISO-8601 report names sort chronologically; sort|tail avoids the empty-dir
    # `xargs ls -t` foot-gun (with no matches, xargs would ls the CWD).
    latest="$(find "$REPORT_DIR" -maxdepth 1 -name '*.json' ! -name '*-summary*' -type f 2>/dev/null | sort | tail -1)"
    printf '%s' "$latest"
}

# ── Entry point ─────────────────────────────────────────────────────────────

# ── Sentinel for test harness extraction ──────────────────────────────────
# If you move this block, update any `src_fn` harness that sources everything
# before the marker. The sentinel must appear immediately before main() entry.

# HARNESS_ENTRY_POINT

main() {
    # WI#410: re-check the WI#376 safe-unit guard on EVERY root-cron tick, not
    # only at arming time (ic/scripts/enable-health-fleet.sh). Fail closed
    # before any probe, restart, or state mutation: refuse_openrc_tick pages
    # (ID-1), takes no lock, writes no state, and no rc-service runs.
    if [[ "$SERVICE_MANAGER" == "openrc" ]]; then
        if [[ "$OPENRC_PROXY_CONTRACT_MISSING" == true ]]; then
            refuse_openrc_tick "openrc-proxy-unit-contract.sh is missing beside lunarwing-self-heal.sh; refusing to probe or restart services as root without the WI#376 safe-unit contract (WI#410); re-sync the health lib: ic/scripts/lunarwing-mt-admin.sh upgrade-tenant <tenant> (or ic/scripts/enable-health-fleet.sh)" "contract_missing"
        fi
        local offender=""
        offender="$(first_unsafe_openrc_proxy_unit "$OPENRC_INITD_DIR")" || {
            refuse_openrc_tick "unsafe legacy proxy unit ${offender} installed; refusing to probe or restart services as root (WI#410); run: ic/scripts/lunarwing-mt-admin.sh render-units --all-proxies (a stale unit of a removed tenant must first be moved outside init.d by hand)" "unsafe_proxy_unit:${offender##*/}"
        }
    fi

    local report="$REPORT_FILE"
    local now_epoch; now_epoch="$(date +%s)"

    ensure_state_dir

    local lockfile="$SELF_HEAL_STATE_DIR/self-heal.lock"
    if command -v flock >/dev/null 2>&1; then
        exec 200>"$lockfile"
        if ! flock -n 200; then
            log "WARNING: another self-heal instance is running; exiting"
            exit 0
        fi
    fi

    [[ -z "$report" ]] && report="$(find_latest_report)"
    [[ -z "$report" || ! -f "$report" ]] && die "no health-check report found in $REPORT_DIR"

    # Staleness guard: don't remediate on stale data (e.g. scheduler stalled).
    if [[ "$MAX_REPORT_AGE" -gt 0 ]] 2>/dev/null; then
        local mtime; mtime="$(stat -c %Y "$report" 2>/dev/null || stat -f %m "$report" 2>/dev/null || echo "$now_epoch")"
        local report_age=$(( now_epoch - mtime ))
        if [[ "$report_age" -gt "$MAX_REPORT_AGE" ]]; then
            log "WARNING: latest report is ${report_age}s old (> MAX_REPORT_AGE=${MAX_REPORT_AGE}s); refusing to act on stale data"
            exit 0
        fi
    fi

    log "=== Self-Healing Watchdog v$VERSION ==="
    log "report: $report"
    log "service manager: $SERVICE_MANAGER"
    log "config: max-retries=$MAX_RETRIES settle=${BACKOFF_SECONDS}s backoff=$BACKOFF_STRATEGY base=${BACKOFF_BASE}s max=${BACKOFF_MAX}s grace=$GRACE_CHECKS flap=$FLAP_MAX_RESTARTS/${FLAP_WINDOW_SECS}s prune-ttl=${STATE_PRUNE_TTL}s verify=$VERIFY_HEALTH escalate-cooldown=${ESCALATE_COOLDOWN}s systemd-scope=$SYSTEMD_SCOPE netbird-remediate=$NETBIRD_REMEDIATE dry-run=$DRY_RUN"

    jq . "$report" >/dev/null 2>&1 || die "report is not valid JSON: $report"

    local state
    state="$(load_state)"

    # Build target set (unhealthy) and healthy set (for report-as-truth reset).
    local -a targets=()
    local -A seen=()
    local -A healthy=()
    local comp services svc

    # 1) Standard logical components. MT mode skips tenant-local logical
    # mappings, but host-global monitor-only components remain visible.
    if [[ "$REMEDY_LOGICAL" != "true" ]]; then
        log "MT mode: tenant-local logical-component remediation disabled (SELF_HEAL_REMEDY_LOGICAL=false); host-global monitoring remains active"
    fi
    while IFS=$'\t' read -r comp cstatus; do
        [[ -n "$comp" ]] || continue
        [[ "$REMEDY_LOGICAL" == "true" || "${MONITOR_ONLY[$comp]:-}" == "1" ]] || continue
        # Diagnostic components own no service; they are handled per-instance
        # below (step 1b) with synthetic monitor-only identities.
        [[ -n "${DIAGNOSTIC_COMPONENTS[$comp]:-}" ]] && continue
        [[ "${NO_REMEDY[$comp]:-}" == "1" ]] && { [[ "$cstatus" != healthy ]] && log "SKIP: component '$comp' has no standalone service (feature/external)"; continue; }
        services="${SERVICE_MAP[$comp]:-}"
        [[ -n "$services" ]] || { [[ "$cstatus" != healthy ]] && log "WARNING: no service mapping for component '$comp'; skipping"; continue; }
        for svc in $services; do
            if [[ "$cstatus" == healthy ]]; then
                healthy[$svc]=1
            else
                [[ -n "${seen[$svc]:-}" ]] && continue
                seen[$svc]=1
                targets+=("$comp:$svc")
            fi
        done
    done < <(jq -r '.components[] | "\(.component)\t\(.status)"' "$report" 2>/dev/null || true)

    # 1b) Monitor-only diagnostic components (runtime, knowledge — MR !1041
    # review finding 2): per-instance failures become synthetic monitor-only
    # targets ("<comp>@<tenant>", or bare "<comp>" for unattributed/legacy
    # single-node results) so they grace-track and escalate with tenant
    # attribution even when MT logical remediation is disabled. Healthy
    # instances clear state through the report-as-truth path; unprobed/unknown
    # instances are left to the component-level verdict (coverage notes must
    # not page per tenant).
    local diag_comp inst_tenant inst_status diag_id diag_cstatus
    local -a diag_bad=()
    for diag_comp in runtime knowledge; do
        diag_bad=()
        while IFS=$'\t' read -r inst_tenant inst_status; do
            [[ -n "$inst_tenant" ]] || continue
            if [[ "$inst_tenant" == "default" ]]; then
                diag_id="$diag_comp"
            else
                diag_id="${diag_comp}@${inst_tenant}"
            fi
            case "$inst_status" in
                healthy) healthy[$diag_id]=1 ;;
                degraded|critical)
                    [[ -n "${seen[$diag_id]:-}" ]] && continue
                    seen[$diag_id]=1
                    diag_bad+=("$diag_id")
                    targets+=("$diag_comp:$diag_id")
                    ;;
                *) ;;
            esac
        done < <(jq -r --arg comp "$diag_comp" '
            .components[] | select(.component == $comp)
            | (.metrics.instances // [])[]
            | "\(.tenant // "default")\t\(.status // "unknown")"
        ' "$report" 2>/dev/null || true)
        # Component-level failure with no failing instance (invalid registry,
        # incomplete sweep, unknown check outcome): attribute the component
        # itself so the monitoring gap still escalates. Only explicit failure
        # statuses qualify — an unparseable/absent component creates no
        # target.
        diag_cstatus="$(jq -r --arg comp "$diag_comp" \
            '[.components[] | select(.component == $comp) | .status] | first // "absent"' \
            "$report" 2>/dev/null || true)"
        if [[ ${#diag_bad[@]} -eq 0 ]] && [[ "$diag_cstatus" == degraded || "$diag_cstatus" == critical || "$diag_cstatus" == unknown ]]; then
            [[ -n "${seen[$diag_comp]:-}" ]] && continue
            seen[$diag_comp]=1
            targets+=("$diag_comp:$diag_comp")
        fi
    done

    # 2) Init-system sub-units (systemd/openrc/launchd). Healthy ones populate
    #    `healthy`; unhealthy ones become targets.
    local init_comp init_status init_name svc_norm
    while IFS=$'\t' read -r init_comp init_status init_name; do
        [[ -n "$init_name" ]] || continue
        svc_norm="$init_name"
        [[ "$SERVICE_MANAGER" == "openrc" ]] && svc_norm="${svc_norm%.service}"
        [[ "$SERVICE_MANAGER" == "systemd" && "$svc_norm" != *.* ]] && svc_norm="${svc_norm}.service"
        case "$init_status" in
            healthy) healthy[$svc_norm]=1 ;;
            skipped) log "SKIP: $svc_norm reported not-started/disabled (F3); not remediating" ;;
            *)
                [[ -n "${seen[$svc_norm]:-}" ]] && continue
                seen[$svc_norm]=1
                targets+=("$init_comp:$svc_norm") ;;
        esac
    # NOTE: each init-system alternative MUST be fully parenthesized — jq's `|`
    # binds looser than `,`, so an unparenthesized trailing string gets piped
    # into the next alternative's `.metrics` and aborts the filter.
    done < <(jq -r '
        ( .components[] | select(.component == "systemd") | (.metrics.units // [])[]    | "systemd\t\(.status)\t\(.name)" ),
        ( .components[] | select(.component == "openrc")  | (.metrics.services // [])[] | "openrc\t\(.status)\t\(.name)" ),
        ( .components[] | select(.component == "launchd") | (.metrics.agents // [])[]   | "launchd\t\(.status)\t\(.name)" )
    ' "$report" 2>/dev/null || true)

    # Report-as-truth recovery: a service the report now calls healthy is cleared
    # without round-tripping through check_service_active (flaky right after a
    # restart). Services that simply aren't in this report are left for the TTL prune.
    local k
    while IFS= read -r k; do
        [[ -n "$k" ]] || continue
        if [[ -n "${healthy[$k]:-}" ]]; then
            state="$(clear_service_state "$state" "$k")"
            log "RECOVERED: cleared state for $k (report says healthy)"
        fi
    done < <(echo "$state" | jq -r 'keys[]' 2>/dev/null || true)

    if [[ ${#targets[@]} -eq 0 ]]; then
        state="$(prune_state "$state" "$now_epoch")"
        persist_state "$state"
        log "=== Self-Healing complete (nothing to do) ==="
        exit 0
    fi

    log "identified ${#targets[@]} target(s) for remediation"

    local target
    for target in "${targets[@]}"; do
        IFS=: read -r comp svc <<< "$target"
        state="$(remediate_component "$comp" "$state" "$svc")"
    done

    state="$(prune_state "$state" "$now_epoch" "${!seen[@]}")"
    persist_state "$state"

    local summary
    summary="$(echo "$state" | jq -r 'to_entries | map({service: .key, retries: (.value.retries // 0), escalated: (.value.escalated // false)})')"
    log "=== Self-Healing complete ==="
    log "state: $summary"
}

main "$@"
exit 0
