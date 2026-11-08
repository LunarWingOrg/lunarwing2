#!/usr/bin/env bash
# WI#586 disk health fixtures. No host df/stat results or services are used.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CHECK="$ROOT/health-disk.sh"
FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT
BIN="$FIXTURE/bin"
mkdir -p "$BIN" "$FIXTURE/base"
failures=0
passes=0
pass() { printf 'PASS: %s\n' "$1"; passes=$((passes + 1)); }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }
assert_eq() {
  if [[ "$1" == "$2" ]]; then pass "$3"; else fail "$3 (got '$1', want '$2')"; fi
}
assert_contains() {
  if [[ "$1" == *"$2"* ]]; then pass "$3"; else fail "$3 (missing: $2)"; fi
}
assert_absent() {
  if [[ "$1" != *"$2"* ]]; then pass "$3"; else fail "$3 (unexpected: $2)"; fi
}

cat >"$BIN/stat" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${FI_STAT_LOG:-/dev/null}"
if [[ " $* " != *" -L "* ]]; then
  printf 'stat: -L is required\n' >&2
  exit 64
fi
path="${!#}"
[[ "$path" == "${FI_STAT_HANG:-}" ]] && sleep 30
if [[ "$path" == "${FI_STAT_MISSING:-}" ]]; then
  printf "stat: cannot statx '%s': No such file or directory\n" "$path" >&2
  exit 1
fi
if [[ "$path" == "${FI_STAT_DENIED:-}" ]]; then
  printf "stat: cannot statx '%s': Permission denied\n" "$path" >&2
  exit 1
fi
[[ "$path" == "${FI_STAT_FAIL:-}" ]] && exit 1
if [[ -n "${FI_SYMLINK_PATH:-}" && ( "$path" == "$FI_SYMLINK_PATH" || "$path" == "${FI_SYMLINK_TARGET:-}" ) ]]; then
  printf '2\n'
elif [[ -n "${FI_SECOND_PATH:-}" && "$path" == "$FI_SECOND_PATH" ]]; then
  printf '2\n'
else
  printf '1\n'
fi
STUB
cat >"$BIN/df" <<'STUB'
#!/usr/bin/env bash
[[ "${FI_DF_HANG:-}" == "1" ]] && sleep 30
if [[ " $* " == *" --output="* ]] && [[ " $* " == *" -P "* || " $* " == *" -i "* ]]; then
  printf 'df: options -P/-i and --output are mutually exclusive\n' >&2
  exit 1
fi
path="${!#}"
mount="${FI_MOUNT:-/}"
used="${FI_USED:-10}"
inodes="${FI_INODES:-10}"
size="${FI_SIZE:-107374182400}"
free="${FI_FREE:-10737418240}"
if [[ -n "${FI_SYMLINK_PATH:-}" && ( "$path" == "$FI_SYMLINK_PATH" || "$path" == "${FI_SYMLINK_TARGET:-}" ) ]]; then
  mount="${FI_SYMLINK_MOUNT:-/home}"
elif [[ -n "${FI_SECOND_PATH:-}" && "$path" == "$FI_SECOND_PATH" ]]; then
  mount="${FI_SECOND_MOUNT:-/home}"
  used="${FI_SECOND_USED:-10}"
  inodes="${FI_SECOND_INODES:-10}"
  size="${FI_SECOND_SIZE:-107374182400}"
  free="${FI_SECOND_FREE:-10737418240}"
fi
if [[ " $* " == *" --output=itotal,iavail,ipcent,target "* ]]; then
  printf '     inodes        free    %%iused mounted on\n'
  printf '%s %s %s%% %s\n' "${FI_INODE_TOTAL:-1000000}" "${FI_INODE_AVAIL:-900000}" "$inodes" "$mount"
else
  printf '     1B-blocks        free capacity mounted on\n'
  printf '%s %s %s%% %s\n' "$size" "$free" "$used" "$mount"
fi
STUB
cat >"$BIN/getent" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "passwd" && "${2:-}" == "alpha" ]]; then
  printf 'alpha:x:1001:1001::/home/alpha:/bin/sh\n'
fi
STUB
chmod +x "$BIN"/*

run_check() {
  local base_env=("LUNARWING_BASE_DIR=$FIXTURE/base")
  if [[ "${FI_BASE_DEFAULT:-}" == "1" ]]; then
    base_env=()
  fi
  set +e
  OUTPUT="$(env PATH="$BIN:$PATH" HOME="$FIXTURE/home" "${base_env[@]}" \
    SELF_HEAL_TENANTS_FILE="${FI_TENANTS:-$FIXTURE/missing.json}" \
    HEALTH_DISK_RUNTIME="${FI_RUNTIME:-none}" \
    HEALTH_DISK_EXTRA_PATHS="${FI_EXTRA:-}" \
    HEALTH_DISK_TIMEOUT="${FI_TIMEOUT:-3}" \
    HEALTH_DISK_SWEEP_DEADLINE_SECS="${FI_SWEEP:-19}" \
    HEALTH_DISK_DEGRADED_USED_PCT="${FI_DEGRADED_PCT:-80}" \
    HEALTH_DISK_CRITICAL_USED_PCT="${FI_CRITICAL_PCT:-90}" \
    HEALTH_DISK_DEGRADED_FREE_GIB="${FI_DEGRADED_FREE:-5}" \
    HEALTH_DISK_CRITICAL_FREE_GIB="${FI_CRITICAL_FREE:-2}" \
    FI_STAT_FAIL="${FI_STAT_FAIL:-}" FI_DF_HANG="${FI_DF_HANG:-}" \
    FI_STAT_MISSING="${FI_STAT_MISSING:-}" FI_STAT_DENIED="${FI_STAT_DENIED:-}" \
    FI_STAT_HANG="${FI_STAT_HANG:-}" FI_STAT_LOG="${FI_STAT_LOG:-}" \
    FI_USED="${FI_USED:-10}" FI_INODES="${FI_INODES:-10}" \
    FI_SIZE="${FI_SIZE:-107374182400}" FI_FREE="${FI_FREE:-10737418240}" \
    FI_INODE_TOTAL="${FI_INODE_TOTAL:-1000000}" FI_INODE_AVAIL="${FI_INODE_AVAIL:-900000}" \
    FI_MOUNT="${FI_MOUNT:-}" FI_SYMLINK_PATH="${FI_SYMLINK_PATH:-}" \
    FI_SYMLINK_TARGET="${FI_SYMLINK_TARGET:-}" FI_SYMLINK_MOUNT="${FI_SYMLINK_MOUNT:-}" \
    FI_SECOND_PATH="${FI_SECOND_PATH:-}" FI_SECOND_MOUNT="${FI_SECOND_MOUNT:-}" \
    FI_SECOND_USED="${FI_SECOND_USED:-10}" FI_SECOND_INODES="${FI_SECOND_INODES:-10}" \
    FI_SECOND_SIZE="${FI_SECOND_SIZE:-107374182400}" FI_SECOND_FREE="${FI_SECOND_FREE:-10737418240}" \
    "$CHECK" 2>"$FIXTURE/stderr.log")"
  RC=$?
  set -e
}

run_check
assert_eq "$RC" "0" "healthy filesystem exits zero"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "healthy" "healthy filesystem reports healthy"
assert_eq "$(jq -r '.metrics.filesystems[0].used_pct' <<<"$OUTPUT")" "10" "block percentage is exported"
assert_eq "$(jq -r '.metrics.filesystems[0].inodes_used_pct' <<<"$OUTPUT")" "10" "inode percentage is exported"
assert_eq "$(jq -r '.metrics.filesystems[0].free_bytes' <<<"$OUTPUT")" "10737418240" "free bytes are exported"
assert_eq "$(jq '.metrics.filesystems | length' <<<"$OUTPUT")" "1" "default paths on one device are deduplicated"

FI_INODES=- FI_INODE_TOTAL=0 FI_INODE_AVAIL=0 run_check
assert_eq "$RC" "0" "untracked inodes do not degrade healthy blocks"
assert_eq "$(jq -r '.metrics.filesystems[0].inodes_used_pct' <<<"$OUTPUT")" "null" "untracked inodes are exported as null"
assert_eq "$(jq -r '.metrics.filesystems[0].available' <<<"$OUTPUT")" "true" "block metrics remain available without inode accounting"

FI_USED=80 run_check
assert_eq "$RC" "1" "80 percent blocks is degraded"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "degraded" "degraded block usage drives component status"
assert_contains "$OUTPUT" '80% blocks' "block issue names the percentage and mount"

FI_USED=90 run_check
assert_eq "$RC" "2" "90 percent blocks is critical"
assert_eq "$(jq -r '.metrics.filesystems[0].status' <<<"$OUTPUT")" "critical" "critical block usage is attributed to the filesystem"

FI_INODES=90 run_check
assert_eq "$RC" "2" "inode exhaustion is critical with free blocks"
assert_contains "$OUTPUT" '90% inodes' "inode issue names the inode percentage"

FI_FREE=4294967296 run_check
assert_eq "$RC" "1" "less than 5 GiB free is degraded"
FI_FREE=1073741824 run_check
assert_eq "$RC" "2" "less than 2 GiB free is critical"
FI_FREE=2147483648 run_check
assert_eq "$RC" "1" "exactly 2 GiB free is degraded rather than critical"
FI_FREE=5368709120 run_check
assert_eq "$RC" "0" "exactly 5 GiB free does not cross the strict degraded floor"

FI_USED=10 FI_DEGRADED_PCT=08 run_check
assert_eq "$RC" "1" "leading-zero thresholds are interpreted in base ten"
assert_absent "$(cat "$FIXTURE/stderr.log")" 'value too great for base' "leading-zero thresholds never fail open"

FI_STAT_FAIL=/tmp run_check
assert_eq "$RC" "1" "an unstatable monitored path is degraded"
assert_eq "$(jq -r '.metrics.filesystems[] | select(.mount=="/tmp") | .available' <<<"$OUTPUT")" "false" "unavailable path is explicit in metrics"
assert_contains "$OUTPUT" "path cannot be stat'ed" "unavailable path issue is diagnostic"

cat >"$FIXTURE/ports.json" <<'JSON'
{"tenants":{"alpha":{"user":"alpha"}}}
JSON
FI_TENANTS="$FIXTURE/ports.json" FI_EXTRA="/srv/runner /var/tmp" run_check
assert_eq "$RC" "0" "tenant and extra paths can be healthy"
assert_eq "$(jq '.metrics.filesystems | length' <<<"$OUTPUT")" "1" "tenant and operator paths deduplicate by device"
assert_contains "$(jq -c '.metrics.filesystems[0].paths' <<<"$OUTPUT")" '/home/alpha/.local/share/containers/storage' "tenant store comes from registry user home"
assert_contains "$(jq -c '.metrics.filesystems[0].paths' <<<"$OUTPUT")" '/srv/runner' "operator extra path is monitored"
assert_eq "$(jq '[.metrics.filesystems[0].paths[] | select(.=="/var/tmp")] | length' <<<"$OUTPUT")" "1" "an already-watched extra path is not duplicated"

tenant_store=/home/alpha/.local/share/containers/storage
FI_TENANTS="$FIXTURE/ports.json" FI_STAT_MISSING="$tenant_store" run_check
assert_eq "$RC" "0" "an absent discovered tenant store stays healthy"
assert_eq "$(jq -r --arg path "$tenant_store" '.metrics.filesystems[] | select(.mount==$path) | .present' <<<"$OUTPUT")" "false" "absent tenant store is explicit in metrics"
assert_eq "$(jq -r --arg path "$tenant_store" '.metrics.filesystems[] | select(.mount==$path) | .status' <<<"$OUTPUT")" "healthy" "absent tenant store has no failure verdict"
assert_absent "$OUTPUT" "path cannot be stat'ed" "absent discovered paths do not create issues"

FI_TENANTS="$FIXTURE/ports.json" FI_STAT_DENIED="$tenant_store" run_check
assert_eq "$RC" "1" "an unreadable discovered tenant store is degraded"
assert_eq "$(jq -r --arg path "$tenant_store" '.metrics.filesystems[] | select(.mount==$path) | .available' <<<"$OUTPUT")" "false" "unreadable tenant store is unavailable"

FI_TENANTS="$FIXTURE/ports.json" FI_STAT_HANG="$tenant_store" FI_TIMEOUT=1 run_check
assert_eq "$RC" "1" "a timed-out discovered tenant store is degraded"

FI_EXTRA=/srv/runner FI_STAT_MISSING=/srv/runner run_check
assert_eq "$RC" "1" "an absent operator-extra path remains strict"

FI_RUNTIME=podman FI_STAT_MISSING=/var/lib/containers/storage run_check
assert_eq "$RC" "0" "an absent discovered root store stays healthy"
assert_eq "$(jq -r '.metrics.filesystems[] | select(.mount=="/var/lib/containers/storage") | .present' <<<"$OUTPUT")" "false" "absent root store is explicit in metrics"

FI_BASE_DEFAULT=1 FI_STAT_MISSING="$FIXTURE/home/.lunarwing" run_check
assert_eq "$RC" "0" "an absent default base directory stays healthy"
assert_eq "$(jq -r --arg path "$FIXTURE/home/.lunarwing" '.metrics.filesystems[] | select(.mount==$path) | .present' <<<"$OUTPUT")" "false" "absent default base is explicit in metrics"

: >"$FIXTURE/stat.log"
FI_SYMLINK_PATH=/alias FI_SYMLINK_TARGET=/target FI_SYMLINK_MOUNT=/home \
  FI_EXTRA="/alias /target" FI_STAT_LOG="$FIXTURE/stat.log" run_check
assert_eq "$RC" "0" "symlink and target paths can be healthy"
assert_eq "$(jq '[.metrics.filesystems[] | select(.mount=="/home")] | length' <<<"$OUTPUT")" "1" "stat -L deduplicates symlink targets by device"
assert_eq "$(jq '.metrics.filesystems | length' <<<"$OUTPUT")" "2" "symlink target numbers stay on a separate device"
assert_contains "$(jq -c '.metrics.filesystems[] | select(.mount=="/home") | .paths' <<<"$OUTPUT")" '/alias' "symlink path joins the target filesystem"
assert_contains "$(jq -c '.metrics.filesystems[] | select(.mount=="/home") | .paths' <<<"$OUTPUT")" '/target' "target path joins the same filesystem"
assert_contains "$(cat "$FIXTURE/stat.log")" '-L' "device probes always follow symlinks"

FI_RUNTIME=both run_check
assert_contains "$(jq -c '.metrics.filesystems[0].paths' <<<"$OUTPUT")" '/var/lib/containers/storage' "podman root store is discovered by path"
assert_contains "$(jq -c '.metrics.filesystems[0].paths' <<<"$OUTPUT")" '/var/lib/docker' "Docker root store is discovered by path"

FI_USED=85 FI_FREE=4294967296 FI_DEGRADED_PCT=bad FI_CRITICAL_PCT=bad \
  FI_DEGRADED_FREE=bad FI_CRITICAL_FREE=bad run_check
assert_eq "$RC" "1" "invalid knobs fall back to the default degraded thresholds"
assert_contains "$(cat "$FIXTURE/stderr.log")" 'using 80' "invalid percentage fallback is logged to stderr"

FI_USED=85 FI_DEGRADED_PCT=95 FI_CRITICAL_PCT=90 run_check
assert_eq "$RC" "1" "reversed percentage thresholds fall back to defaults"
assert_contains "$(cat "$FIXTURE/stderr.log")" 'using 80 and 90' "reversed percentage warning is diagnostic"
FI_FREE=4294967296 FI_DEGRADED_FREE=1 FI_CRITICAL_FREE=2 run_check
assert_eq "$RC" "1" "reversed free-space floors fall back to defaults"
assert_contains "$(cat "$FIXTURE/stderr.log")" 'using 5 and 2' "reversed free-space warning is diagnostic"

FI_DF_HANG=1 FI_TIMEOUT=1 SECONDS=0 run_check
assert_eq "$RC" "1" "a hung df is cut off and degraded"
assert_contains "$OUTPUT" 'block df failed or timed out' "hung df produces a bounded-probe issue"
if ((SECONDS < 6)); then pass "hung df returns within the per-probe budget"; else fail "hung df exceeded the per-probe budget ($SECONDS seconds)"; fi

FI_DF_HANG=1 FI_TIMEOUT=3 FI_SWEEP=1 SECONDS=0 run_check
assert_eq "$RC" "1" "an exhausted sweep deadline returns a degraded partial report"
assert_contains "$OUTPUT" 'disk sweep incomplete' "sweep deadline coverage loss is explicit"
if ((SECONDS < 6)); then pass "overall deadline lands before the outer component timeout"; else fail "overall deadline exceeded its budget ($SECONDS seconds)"; fi

self_heal="$ROOT/lunarwing-self-heal.sh"
assert_contains "$(cat "$self_heal")" 'MONITOR_ONLY[disk]=1' "disk is pinned monitor-only"
report="$FIXTURE/disk-report.json"
cat >"$report" <<'JSON'
{"timestamp":"2026-11-08T05:00:00Z","overall_status":"critical","components":[
 {"component":"disk","status":"critical","metrics":{"filesystems":[
  {"mount":"/","paths":["/"],"size_bytes":100,"free_bytes":50,"used_pct":10,"inodes_used_pct":10,"status":"healthy","available":true},
  {"mount":"/home","paths":["/home/alpha/.local/share/containers/storage"],"size_bytes":100,"free_bytes":1,"used_pct":95,"inodes_used_pct":10,"status":"critical","available":true}
 ]},"issues":["disk@/home critical"]}],"alerts":[]}
JSON
cat >"$FIXTURE/notify.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$NOTIFY_LOG"
STUB
chmod +x "$FIXTURE/notify.sh"
set +e
SELF_OUTPUT="$(env PATH="$BIN:$PATH" NOTIFY_LOG="$FIXTURE/notify.log" \
  LUNARWING_SERVICE_MANAGER=systemd SELF_HEAL_REMEDY_LOGICAL=false \
  SELF_HEAL_STATE_DIR="$FIXTURE/self-heal" SELF_HEAL_NOTIFY_SCRIPT="$FIXTURE/notify.sh" \
  "$self_heal" --report "$report" --grace-checks 1 2>&1)"
SELF_RC=$?
set -e
assert_eq "$SELF_RC" "0" "self-heal handles a disk-only report"
assert_contains "$SELF_OUTPUT" 'ESCALATE target=disk@/home retries=0 reason=monitor_only' "filesystem failure escalates with its mount key"
assert_contains "$(cat "$FIXTURE/notify.log")" 'critical' "disk escalation reaches the notifier"
assert_eq "$(jq -r '.["disk@/home"].escalated' "$FIXTURE/self-heal/state.json")" "true" "delivered disk page is recorded"
assert_absent "$SELF_OUTPUT" 'RESTART_BEGIN target=disk@' "disk escalation never attempts a service restart"

metrics_dir="$FIXTURE/metrics-reports"
mkdir -p "$metrics_dir"
cp "$report" "$metrics_dir/2026-11-08T05:00:00Z.json"
env LUNARWING_BASE_DIR="$FIXTURE" HEALTH_REPORT_DIR="$metrics_dir" \
  HEALTH_METRICS_OUTPUT="$FIXTURE/metrics.prom" HEALTH_METRICS_INCLUDE_SELFHEAL=false \
  "$ROOT/health-metrics-exporter.sh" >/dev/null 2>&1
metrics="$(cat "$FIXTURE/metrics.prom")"
assert_contains "$metrics" 'lunarwing_health_disk_used_pct{mount="/home"} 95' "disk block gauge is exported per mount"
assert_contains "$metrics" 'lunarwing_health_disk_inodes_used_pct{mount="/home"} 10' "disk inode gauge is exported per mount"
assert_contains "$metrics" 'lunarwing_health_disk_free_bytes{mount="/home"} 1' "disk free-space gauge is exported per mount"
assert_contains "$metrics" 'lunarwing_health_disk_filesystem_status{mount="/home"} 2' "per-filesystem status gauge is exported"
assert_absent "$metrics" 'paths=' "path lists do not become Prometheus labels"

assert_contains "$(cat "$ROOT/infrastructure-health-check.sh")" 'run_check "health-disk.sh" "disk"' "main health check runs the disk component"
assert_contains "$(cat "$SCRIPT_DIR/../../ic/scripts/mt-admin-lib/12-health-pipeline.sh")" '#HEALTH_DISK_EXTRA_PATHS=' "health.env documents operator extra paths"

printf '\n%d passed, %d failed\n' "$passes" "$failures"
[[ "$failures" -eq 0 ]]
