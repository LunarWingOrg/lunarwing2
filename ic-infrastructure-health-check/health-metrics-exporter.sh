#!/bin/bash
# LunarWing Healthpoint Metrics Exporter
#
# Converts the newest aggregated health report (and the self-heal state file)
# into Prometheus text exposition format and atomically writes it to a
# node_exporter / VictoriaMetrics textfile-collector target.
#
# Design contract (docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md):
#   - Textfile first: no listener, no port, read-only side-channel.
#   - The report is the single source of truth; this script never probes a
#     service itself, so every healthpoint is covered without per-check work.
#   - Only numeric/boolean leaves are exported. Strings are dropped, so no
#     secret material (URLs, tokens, versions) can ever reach the exposition.
#   - Tenant labels come from the report itself (instances carry .tenant) —
#     never from a hardcoded port.
#   - Absent self-heal state or thresholds file is not an error: those series
#     are simply omitted.
#
# Usage:
#   ./health-metrics-exporter.sh
#
# Environment (all optional, defaults safe):
#   HEALTH_REPORT_DIR                     default: <base>/workspace/reports/health
#   HEALTH_METRICS_OUTPUT                 default: <report dir>/metrics.prom
#   HEALTH_METRICS_INCLUDE_SELFHEAL       default: true
#   HEALTH_METRICS_SELFHEAL_STATE         default: <report dir>/../self-heal/state.json
#   HEALTH_METRICS_THRESHOLDS_FILE        default: /etc/lunarwing/health-metrics-thresholds.conf
#   HEALTH_METRICS_EARLY_WARNING_NOTIFY   default: false (deduplicated Gotify warning)
#   HEALTH_METRICS_NOTIFY_COOLDOWN        default: SELF_HEAL_ESCALATE_COOLDOWN or 3600
#   SELF_HEAL_FLAP_WINDOW_SECS            default: 3600 (window for the flapping gauge)
#   LUNARWING_BASE_DIR / IRONCLAW_BASE_DIR  default: $HOME/.lunarwing

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

BASE_DIR="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}"
REPORT_DIR="${HEALTH_REPORT_DIR:-$BASE_DIR/workspace/reports/health}"
OUTPUT_FILE="${HEALTH_METRICS_OUTPUT:-$REPORT_DIR/metrics.prom}"
SELFHEAL_STATE_FILE="${HEALTH_METRICS_SELFHEAL_STATE:-$REPORT_DIR/../self-heal/state.json}"
INCLUDE_SELFHEAL="${HEALTH_METRICS_INCLUDE_SELFHEAL:-true}"
THRESHOLDS_FILE="${HEALTH_METRICS_THRESHOLDS_FILE:-/etc/lunarwing/health-metrics-thresholds.conf}"
EARLY_WARNING_NOTIFY="${HEALTH_METRICS_EARLY_WARNING_NOTIFY:-false}"
NOTIFY_COOLDOWN="${HEALTH_METRICS_NOTIFY_COOLDOWN:-${SELF_HEAL_ESCALATE_COOLDOWN:-3600}}"
FLAP_WINDOW_SECS="${SELF_HEAL_FLAP_WINDOW_SECS:-3600}"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/lunarwing-metrics.XXXXXX")"
trap 'rm -rf "$WORKDIR"' EXIT

log() { echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

usage() {
    sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
}
[[ "${1:-}" == "-h" || "${1:-}" == "--help" ]] && usage

command -v jq >/dev/null 2>&1 || die "jq is required"

# ── Locate the newest report ────────────────────────────────────────────────
# Report files are named <ISO-8601>.json (UTC), so lexicographic glob order is
# chronological. No `ls` parsing, no dependence on mtime.
latest_report() {
    local f latest=""
    for f in "$REPORT_DIR"/*.json; do
        [[ -e "$f" ]] || continue
        latest="$f"
    done
    [[ -n "$latest" ]] && printf '%s\n' "$latest"
}

# ── Exposition helpers (jq) ─────────────────────────────────────────────────
# esc  : sanitize a metric/label name fragment to [a-zA-Z0-9_:]
# lv   : escape a label VALUE (backslash, quote, newline)
# norm : coerce numeric-looking strings to numbers (health scripts emit several
#        metrics as strings via jq --arg)
# Type guards: gsub on null aborts the whole program, and missing/null keys do
# occur (e.g. units without .extra.tenant_user), so esc/lv must tolerate them.
STATUS_MAP_NOTE='# status codes: 0=healthy 1=degraded 2=critical 3=unknown/skipped'

REPORT_JQ='
def esc: if type == "string" then gsub("[^a-zA-Z0-9_:]"; "_") | (if test("^[0-9]") then "_" + . else . end) else "_" end;
def lv: if type == "string" then gsub("\\\\"; "\\\\") | gsub("\""; "\\\"") | gsub("\n"; "\\n") else "" end;
def status_code: {"healthy":0,"degraded":1,"critical":2}[.] // 3;
def norm: if (type == "string") and test("^-?[0-9]+(\\.[0-9]+)?$") then tonumber else . end;

# Flatten a metrics object into unlabeled series. Structural keys handled
# elsewhere are skipped; non-numeric/non-boolean leaves are dropped.
def toseries($prefix):
  to_entries[]
  | select(.key as $k | (["instances","providers","units","services","agents","enabled"] | index($k)) | not)
  | (.key | esc) as $key
  | (.value | norm) as $v
  | if ($v | type) == "object" then ($v | toseries($prefix + $key + "_"))
    elif ($v | type) == "number" then ($prefix + $key + " " + ($v | tostring))
    elif ($v | type) == "boolean" then ($prefix + $key + " " + (if $v then "1" else "0" end))
    else empty end;

# Per-tenant instance series (lunarvision / lunarvoice): every numeric or
# boolean leaf gets a tenant label; "true"/"false" strings map to 1/0.
def instances($comp):
  (.metrics.instances // [])[] | select(type == "object")
  | ((.tenant // "default") | tostring) as $t
  | ("lunarwing_health_" + $comp) as $base
  | ( . as $i
    | ( $base + "_instance_status{tenant=\"" + ($t | lv) + "\"} "
        + (($i.status // "unknown") | status_code | tostring) ),
      ( $i | to_entries[]
        | select(.key as $k | (["tenant","url","status","service_status","tesseract_version","issues"] | index($k)) | not)
        | (.key | esc) as $key
        | (.value | norm) as $v
        | ($base + "_" + $key + "{tenant=\"" + ($t | lv) + "\"} ") as $head
        | if ($v | type) == "number" then $head + ($v | tostring)
          elif ($v | type) == "boolean" then $head + (if $v then "1" else "0" end)
          elif $v == "true" then $head + "1"
          elif $v == "false" then $head + "0"
          else empty end ) );

# Per-provider stream telemetry. Provider and tenant stay bounded labels rather
# than becoming metric-name fragments, while only numeric/boolean fields export.
def providers($comp):
  (.metrics.providers // [])[] | select(type == "object")
  | ((.provider // "unknown") | tostring) as $p
  | ((.tenant // "default") | tostring) as $t
  | ( . as $provider
    | ( $provider | to_entries[]
      | select(.key as $k | (["provider","tenant"] | index($k)) | not)
      | (.key | esc) as $key
      | (.value | norm) as $v
      | ("lunarwing_health_" + $comp + "_" + $key
         + "{provider=\"" + ($p | lv) + "\",tenant=\"" + ($t | lv) + "\"} ") as $head
      | if ($v | type) == "number" then $head + ($v | tostring)
        elif ($v | type) == "boolean" then $head + (if $v then "1" else "0" end)
        else empty end ) );

# Init-agnostic unit series. systemd emits metrics.units[], openrc
# metrics.services[], launchd metrics.agents[]; shapes differ slightly
# (active_state vs state, optional restart_count / extra.tenant_user).
def units($init):
  [(.metrics.units // []), (.metrics.services // []), (.metrics.agents // [])]
  | flatten(1) | .[] | select(type == "object")
  | ((.name // "unknown") | tostring) as $un
  | (.active_state // .state // "") as $ust
  | ((.extra // {}).tenant_user // "") as $tu
  | (if $tu != "" then ",tenant=\"" + ($tu | lv) + "\"" else "" end) as $tl
  | ( "lunarwing_health_unit_status{init=\"" + $init + "\",unit=\"" + ($un | lv) + "\"" + $tl + "} "
      + ((.status // "unknown") | status_code | tostring)
    , "lunarwing_health_unit_up{init=\"" + $init + "\",unit=\"" + ($un | lv) + "\"" + $tl + "} "
      + (if (($ust == "active") or ((.state // "") == "started")) then "1" else "0" end)
    , ( if has("restart_count")
        then "lunarwing_health_unit_restarts_total{init=\"" + $init + "\",unit=\"" + ($un | lv) + "\"" + $tl + "} "
             + ((.restart_count // 0) | tostring)
        else empty end ) );

. as $rep
| (($rep.timestamp // "") | try fromdateiso8601 catch 0) as $ts
| [ "lunarwing_health_overall_status " + (($rep.overall_status // "unknown") | status_code | tostring)
  , "lunarwing_health_report_timestamp_seconds " + ($ts | tostring)
  , "lunarwing_health_report_age_seconds " + (($NOW | tonumber) - $ts | tostring)
  , "lunarwing_health_alerts_total " + (($rep.alerts // []) | length | tostring)
  ]
  + [ ($rep.components // [])[]
    | . as $c
    | ($c.metrics // {}) as $m
    | (($c.component // "unknown") | tostring | esc) as $comp
    | if ($m.enabled == false)
      then [ "lunarwing_health_component_enabled{component=\"" + $comp + "\"} 0" ]
      else [ "lunarwing_health_component_enabled{component=\"" + $comp + "\"} 1"
             , "lunarwing_health_status{component=\"" + $comp + "\"} "
               + (($c.status // "unknown") | status_code | tostring) ]
        + ( if ($c.duration_seconds // null) != null
            then [ "lunarwing_health_check_duration_seconds{component=\"" + $comp + "\"} "
                   + ($c.duration_seconds | tostring) ]
            else [] end )
        + ($m | [toseries("lunarwing_health_" + $comp + "_")])
        + ($c | [instances($comp)])
        + ($c | [providers($comp)])
        + ($c | [units((($c.component // "unknown") | tostring))])
      end ]
  | flatten
  | join("\n")
'

SELFHEAL_JQ='
def lv: if type == "string" then gsub("\\\\"; "\\\\") | gsub("\""; "\\\"") | gsub("\n"; "\\n") else "" end;
def num($v): ($v // 0);
. as $st
| ($NOW | tonumber) as $now
| [ $st | to_entries[]
  | .key as $svc
  | (.value // {}) as $v
  | ( "lunarwing_health_selfheal_restarts_total{service=\"" + ($svc | lv) + "\"} " + (num($v.retries) | tostring)
    , "lunarwing_health_selfheal_escalated{service=\"" + ($svc | lv) + "\"} "
      + (if ($v.escalated == true) then "1" else "0" end)
    , "lunarwing_health_selfheal_consecutive_unhealthy{service=\"" + ($svc | lv) + "\"} "
      + (num($v.consecutive_unhealthy) | tostring)
    , "lunarwing_health_selfheal_flapping{service=\"" + ($svc | lv) + "\"} "
      + ([($v.restart_history // [])[] | select(. >= ($now - $WINDOW))] | length | tostring)
    , ( if ($v.next_attempt_at // null) != null
        then "lunarwing_health_selfheal_backoff_next_attempt_seconds{service=\"" + ($svc | lv) + "\"} "
             + ($v.next_attempt_at | tostring)
        else empty end )
    ) ]
  | flatten
  | join("\n")
'

# ── Report series ───────────────────────────────────────────────────────────
report_file="$(latest_report)" || true
[[ -n "${report_file:-}" ]] || die "no health report found in $REPORT_DIR"

report_body="$(jq -r --arg NOW "$(date +%s)" \
    "$REPORT_JQ" "$report_file" 2>"$WORKDIR/report-jq.err")" \
    || die "failed to parse health report $report_file: $(cat "$WORKDIR/report-jq.err")"
[[ -n "$report_body" ]] || die "health report $report_file produced no series"

# ── Self-heal series (absent state file => omitted, not an error) ───────────
selfheal_body=""
if [[ "$INCLUDE_SELFHEAL" == "true" ]]; then
    if [[ -f "$SELFHEAL_STATE_FILE" ]]; then
        if ! selfheal_body="$(jq -r --arg NOW "$(date +%s)" --argjson WINDOW "$FLAP_WINDOW_SECS" \
                "$SELFHEAL_JQ" "$SELFHEAL_STATE_FILE" 2>"$WORKDIR/selfheal-jq.err")"; then
            # Corrupt/foreign state must never take the textfile down.
            log "WARNING: unreadable self-heal state at $SELFHEAL_STATE_FILE — series omitted"
            selfheal_body=""
        fi
    fi
fi

# ── Assemble exposition (TYPE lines once, before their samples) ─────────────
assembled="$WORKDIR/full.prom"
{
    echo "# LunarWing healthpoint metrics (generated $(date -u +%Y-%m-%dT%H:%M:%SZ))"
    echo "$STATUS_MAP_NOTE"
    for family in \
        lunarwing_health_overall_status \
        lunarwing_health_report_timestamp_seconds \
        lunarwing_health_report_age_seconds \
        lunarwing_health_alerts_total \
        lunarwing_health_component_enabled \
        lunarwing_health_status \
        lunarwing_health_check_duration_seconds \
        lunarwing_health_unit_status \
        lunarwing_health_unit_up \
        lunarwing_health_unit_restarts_total \
        lunarwing_health_selfheal_restarts_total \
        lunarwing_health_selfheal_escalated \
        lunarwing_health_selfheal_consecutive_unhealthy \
        lunarwing_health_selfheal_flapping \
        lunarwing_health_selfheal_backoff_next_attempt_seconds \
        lunarwing_health_early_warning
    do
        echo "# TYPE $family gauge"
    done
    echo "$report_body"
    [[ -n "$selfheal_body" ]] && printf '%s\n' "$selfheal_body"
} > "$assembled"

# ── Early-warning thresholds (Phase 3) ──────────────────────────────────────
# Sourced shell fragment defining THRESHOLD_<metric>="warn:>=N crit:>M".
# Evaluation happens against the value just exported; a crossing emits a
# lunarwing_health_early_warning{metric,severity} 1 series and (optionally) a
# Gotify warning deduplicated per metric+severity by a cooldown stamp.
cmp_num() { # <a> <op> <b> — exit 0 when a <op> b
    awk -v a="$1" -v op="$2" -v b="$3" 'BEGIN{
        if      (op == ">")  exit !(a > b)
        else if (op == ">=") exit !(a >= b)
        else if (op == "<")  exit !(a < b)
        else if (op == "<=") exit !(a <= b)
        else if (op == "==") exit !(a == b)
        else if (op == "!=") exit !(a != b)
        else exit 1
    }'
}

# Max exported value of a metric across all its label sets ("" if none/none
# numeric). Max rather than last so per-tenant series cannot hide a spike.
# Sample lines are `name value` or `name{labels} value`; comments are skipped
# by anchoring on the metric name in field 1.
metric_max() {
    awk -v m="$1" '
        $1 == m || $1 ~ ("^" m "\\{") {
            v = $NF
            if (v ~ /^-?[0-9]/) print v
        }' "$assembled" 2>/dev/null | sort -g | tail -n1
}

notify_state_dir="$REPORT_DIR/.metrics-notify"
maybe_notify() { # <metric> <severity> <value> <spec>
    [[ "$EARLY_WARNING_NOTIFY" == "true" ]] || return 0
    local key="$1-$2" last now_ts payload_file
    mkdir -p "$notify_state_dir" 2>/dev/null || return 0
    now_ts="$(date +%s)"
    if [[ -f "$notify_state_dir/$key.last" ]]; then
        last="$(cat "$notify_state_dir/$key.last" 2>/dev/null || echo 0)"
        [[ "$last" =~ ^[0-9]+$ ]] || last=0
        (( now_ts - last < NOTIFY_COOLDOWN )) && return 0
    fi
    payload_file="$WORKDIR/warning-$key.json"
    if jq -n --arg svc "early-warning:$1" --arg reason "threshold crossed: $1=$3 (rule: $4)" \
            --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            '{escalated:true, service:$svc, reason:$reason, retries:0,
              action:"investigate before self-heal restarts (early warning)", timestamp:$ts}' \
            > "$payload_file" 2>/dev/null \
        && [[ -x "$SCRIPT_DIR/send-notification.sh" ]]; then
        "$SCRIPT_DIR/send-notification.sh" "degraded" "$payload_file" \
            && printf '%s\n' "$now_ts" > "$notify_state_dir/$key.last" \
            || log "WARNING: early-warning notification for $1 ($2) not delivered"
    else
        log "WARNING: cannot send early-warning notification (send-notification.sh missing or payload failed)"
    fi
}

evaluate_thresholds() {
    local var spec metric value part sev expr op operand
    [[ -r "$THRESHOLDS_FILE" ]] || return 0
    # shellcheck disable=SC1090  # operator-owned config, same discipline as config.sh
    source "$THRESHOLDS_FILE"
    for var in "${!THRESHOLD_@}"; do
        spec="${!var:-}"
        [[ -n "$spec" ]] || continue
        metric="${var#THRESHOLD_}"
        value="$(metric_max "$metric")"
        [[ "$value" =~ ^-?[0-9]+(\.[0-9]+)?$ ]] || continue   # not exported (yet)
        for part in $spec; do
            sev="${part%%:*}"; expr="${part#*:}"
            [[ "$sev" == "warn" || "$sev" == "crit" ]] || continue
            [[ "$expr" =~ ^(>=|<=|==|!=|>|<)(-?[0-9]+(\.[0-9]+)?)$ ]] || continue
            op="${BASH_REMATCH[1]}"; operand="${BASH_REMATCH[2]}"
            if cmp_num "$value" "$op" "$operand"; then
                echo "lunarwing_health_early_warning{metric=\"$metric\",severity=\"$sev\"} 1"
                maybe_notify "$metric" "$sev" "$value" "$expr"
            fi
        done
    done
}

warning_body="$(evaluate_thresholds)"
if [[ -n "$warning_body" ]]; then
    printf '%s\n' "$warning_body" >> "$assembled"
fi

# ── Atomic publish ──────────────────────────────────────────────────────────
# mktemp in the target directory guarantees the same filesystem for rename.
out_dir="$(dirname "$OUTPUT_FILE")"
mkdir -p "$out_dir" || die "cannot create output directory $out_dir"
tmp_out="$(mktemp "$out_dir/.metrics.prom.XXXXXX")" || die "mktemp failed in $out_dir"
if ! cat "$assembled" > "$tmp_out"; then
    rm -f "$tmp_out"
    die "failed to write temporary metrics file"
fi
# World-readable on purpose (node_exporter textfile-collector convention):
# the exposition contains only non-secret numeric/boolean series, and the
# optional Phase-2 listener runs as an unprivileged dynamic user that must be
# able to read this file.
chmod 0644 "$tmp_out"
mv "$tmp_out" "$OUTPUT_FILE" || { rm -f "$tmp_out"; die "failed to publish $OUTPUT_FILE"; }

log "exported $(grep -cE '^[a-zA-Z_:][a-zA-Z0-9_:]*[ ({]' "$OUTPUT_FILE" 2>/dev/null || echo '?') series -> $OUTPUT_FILE"
exit 0
