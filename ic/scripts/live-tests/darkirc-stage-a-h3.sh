#!/usr/bin/env bash
# Destructive, host-level DarkIRC Stage A H3 validation. Never run from CI.
set -uo pipefail
umask 077

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT_DEFAULT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
ANALYZER="$SCRIPT_DIR/darkirc_h3_analyzer.py"
ACK="--i-understand-this-restarts-live-tenants"
MT="$MT_DEFAULT"
RUN_DIR=""
TENANT_A=""
TENANT_B=""
INIT=""

usage() {
  printf 'Usage: sudo %s %s [--mt-admin PATH] TENANT_A TENANT_B\n' "${0##*/}" "$ACK" >&2
  exit 2
}

die() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  local tenant home exchange
  for tenant in "$TENANT_A" "$TENANT_B"; do
    [[ -n "$tenant" ]] || continue
    home="/home/$tenant"
    exchange="$home/.cache/lunarwing-h3/${RUN_DIR##*/}"
    [[ -n "$RUN_DIR" && "$exchange" == "$home/.cache/lunarwing-h3/h3-"* ]] || continue
    command -v runuser >/dev/null 2>&1 \
      && runuser -u "$tenant" -- rm -rf -- "$exchange" 2>/dev/null || true
  done
  [[ -z "$RUN_DIR" ]] || rm -rf -- "$RUN_DIR"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

[[ "${EUID:-$(id -u)}" -eq 0 ]] || die "must run as root"
[[ "${1:-}" == "$ACK" ]] || usage
shift
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mt-admin) [[ $# -ge 2 ]] || usage; MT="$2"; shift 2 ;;
    --*) usage ;;
    *)
      if [[ -z "$TENANT_A" ]]; then TENANT_A="$1"
      elif [[ -z "$TENANT_B" ]]; then TENANT_B="$1"
      else usage
      fi
      shift
      ;;
  esac
done
[[ -n "$TENANT_A" && -n "$TENANT_B" && "$TENANT_A" != "$TENANT_B" ]] || usage
[[ "$TENANT_A" =~ ^[a-z0-9][a-z0-9-]*$ && "$TENANT_B" =~ ^[a-z0-9][a-z0-9-]*$ ]] \
  || die "tenant names must already be canonical"

for command in python3 jq id getent stat install rm date runuser strace; do
  command -v "$command" >/dev/null 2>&1 || die "missing required tool: $command"
done
[[ -x "$MT" && -f "$ANALYZER" ]] || die "mt-admin or analyzer missing"
[[ -r /proc/self/cmdline && -r /proc/self/environ ]] || die "readable procfs required"

if [[ -e /run/openrc/softlevel ]]; then
  INIT=openrc
  command -v rc-service >/dev/null 2>&1 || die "real OpenRC detected but rc-service missing"
elif [[ -d /run/systemd/system ]]; then
  INIT=systemd
  command -v systemctl >/dev/null 2>&1 || die "real systemd detected but systemctl missing"
  command -v journalctl >/dev/null 2>&1 || die "journalctl missing"
else
  die "host is neither booted real OpenRC nor real systemd"
fi

uid_a="$(id -u "$TENANT_A")" || die "tenant A account missing"
uid_b="$(id -u "$TENANT_B")" || die "tenant B account missing"
gid_a="$(id -g "$TENANT_A")" || die "tenant A group missing"
gid_b="$(id -g "$TENANT_B")" || die "tenant B group missing"
[[ "$uid_a" != "$uid_b" && "$gid_a" != "$gid_b" ]] || die "tenants need distinct UIDs and primary groups"
[[ -d "/home/$TENANT_A/lunarwing/state/darkirc" && -d "/home/$TENANT_B/lunarwing/state/darkirc" ]] \
  || die "DarkIRC state missing"
[[ -f "/home/$TENANT_A/lunarwing/env/darkirc-adapter.env" \
  && -f "/home/$TENANT_B/lunarwing/env/darkirc-adapter.env" ]] || die "DarkIRC secret env missing"

RUN_DIR="/run/lunarwing-darkirc-h3/h3-$(date +%s)-$$"
install -d -o root -g root -m 0700 /run/lunarwing-darkirc-h3 "$RUN_DIR" || die "cannot create secure run directory"
[[ "$(stat -c '%u:%g:%a' "$RUN_DIR")" == "0:0:700" ]] || die "run directory security mismatch"
install -d -m 0700 "$RUN_DIR/commands" "$RUN_DIR/proc" "$RUN_DIR/services" || die "cannot create capture dirs"
strace -f -qq -e trace=execve -e inject=execve:delay_exit=1ms \
  -o "$RUN_DIR/strace-probe" /bin/true \
  || die "strace exec-delay injection is required for deterministic proc sampling"
rm -f -- "$RUN_DIR/strace-probe"

run_capture() {
  local label="$1" pid watcher rc state ready attempts=0
  shift
  bash -c 'kill -STOP "$$"; exec strace -f -qq -v -s 1048576 -e trace=execve -e inject=execve:delay_exit=50ms -o "$1" "${@:2}"' \
    h3-command "$RUN_DIR/proc/$label.strace" "$@" \
    >"$RUN_DIR/commands/$label.stdout" 2>"$RUN_DIR/commands/$label.stderr" &
  pid=$!
  while :; do
    state=""
    read -r _ _ state _ <"/proc/$pid/stat" 2>/dev/null || true
    [[ "$state" == T ]] && break
    [[ -d "/proc/$pid" ]] || die "capture wrapper exited before proc sampling: $label"
    (( attempts += 1 ))
    [[ "$attempts" -lt 1000 ]] || die "capture wrapper did not stop: $label"
    sleep 0.001
  done
  ready="$RUN_DIR/proc/$label.ready"
  python3 "$ANALYZER" watch-proc --pid "$pid" --output "$RUN_DIR/proc/$label.bin" --ready "$ready" &
  watcher=$!
  attempts=0
  while [[ ! -f "$ready" ]]; do
    kill -0 "$watcher" 2>/dev/null || die "proc sampler exited before ready: $label"
    (( attempts += 1 ))
    [[ "$attempts" -lt 1000 ]] || die "proc sampler not ready: $label"
    sleep 0.001
  done
  kill -CONT "$pid" || die "cannot continue captured command: $label"
  wait "$pid"; rc=$?
  wait "$watcher" || die "proc sampler failed for $label"
  [[ "$rc" -eq 0 ]] || die "command $label failed; raw details deleted at exit"
}

json_field() {
  local file="$1" field="$2" value
  value="$(jq -er --arg field "$field" '.[$field] | strings | select(length > 0)' "$file" 2>/dev/null)" \
    || die "missing JSON field $field in ${file##*/}"
  printf '%s' "$value"
}

systemd_user() {
  local tenant="$1" uid="$2"
  shift 2
  [[ -S "/run/user/$uid/bus" ]] || return 1
  runuser -u "$tenant" -- env XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user "$@"
}

verify_services() {
  local tenant="$1" uid="$2" daemon adapter
  daemon="lunarwing-darkirc-$tenant"
  adapter="lunarwing-darkirc-adapter-$tenant"
  if [[ "$INIT" == systemd ]]; then
    systemd_user "$tenant" "$uid" is-active --quiet "$daemon.service" || return 1
    systemd_user "$tenant" "$uid" is-active --quiet "$adapter.service" || return 1
  else
    [[ -x "/etc/init.d/$daemon" && -x "/etc/init.d/$adapter" ]] || return 1
    rc-service "$daemon" status >/dev/null 2>&1 || return 1
    rc-service "$adapter" status >/dev/null 2>&1 || return 1
  fi
}

run_capture preflight-a "$MT" darkirc-contact doctor "$TENANT_A"
run_capture preflight-b "$MT" darkirc-contact doctor "$TENANT_B"
run_capture status-a "$MT" status "$TENANT_A"
run_capture status-b "$MT" status "$TENANT_B"
verify_services "$TENANT_A" "$uid_a" || die "tenant A DarkIRC services/user manager unavailable"
verify_services "$TENANT_B" "$uid_b" || die "tenant B DarkIRC services/user manager unavailable"

python3 "$ANALYZER" probe-peer --user "$TENANT_A" --path "/home/$TENANT_B/lunarwing/state/darkirc" \
  || die "tenant A can access tenant B secret path"
python3 "$ANALYZER" probe-peer --user "$TENANT_B" --path "/home/$TENANT_A/lunarwing/state/darkirc" \
  || die "tenant B can access tenant A secret path"
python3 "$ANALYZER" probe-peer --user "$TENANT_A" --path "/home/$TENANT_B/lunarwing/env/darkirc-adapter.env" \
  || die "tenant A can access tenant B secret env"
python3 "$ANALYZER" probe-peer --user "$TENANT_B" --path "/home/$TENANT_A/lunarwing/env/darkirc-adapter.env" \
  || die "tenant B can access tenant A secret env"

token="${RUN_DIR##*/}"
rel=".cache/lunarwing-h3/$token"
dir_a="/home/$TENANT_A/$rel"
dir_b="/home/$TENANT_B/$rel"
[[ ! -L "/home/$TENANT_A/.cache" && ! -L "/home/$TENANT_B/.cache" ]] || die "tenant cache path is symlinked"
runuser -u "$TENANT_A" -- mkdir -p -- "$dir_a" || die "cannot create tenant A exchange directory"
runuser -u "$TENANT_B" -- mkdir -p -- "$dir_b" || die "cannot create tenant B exchange directory"
chmod 0700 "$dir_a" "$dir_b" || die "cannot secure exchange directories"
[[ "$(stat -c '%u:%g:%a' "$dir_a")" == "$uid_a:$gid_a:700" \
  && "$(stat -c '%u:%g:%a' "$dir_b")" == "$uid_b:$gid_b:700" ]] \
  || die "exchange directory owner/mode mismatch"

started="$(date +%s)"
run_capture restart-a "$MT" restart-tenant "$TENANT_A"
run_capture restart-b "$MT" restart-tenant "$TENANT_B"
verify_services "$TENANT_A" "$uid_a" || die "tenant A services failed controlled restart"
verify_services "$TENANT_B" "$uid_b" || die "tenant B services failed controlled restart"

contact_a="h3-$TENANT_B-${token##*-}"
contact_b="h3-$TENANT_A-${token##*-}"
run_capture prepare-a "$MT" darkirc-contact prepare "$TENANT_A" "$contact_a" --out "$rel/offer-a.json" --json
python3 "$ANALYZER" extract --run-dir "$RUN_DIR" --output "$RUN_DIR/canaries.json" \
  --state-dir "/home/$TENANT_A/lunarwing/state/darkirc" --state-dir "/home/$TENANT_B/lunarwing/state/darkirc" \
  || die "private canary extraction failed"
fp_a="$(json_field "$RUN_DIR/commands/prepare-a.stdout" public_fingerprint)"
id_a="$(json_field "$RUN_DIR/commands/prepare-a.stdout" offer_id)"
python3 "$ANALYZER" copy-public --source "$dir_a/offer-a.json" --dest "$dir_b/offer-a.json" \
  --source-uid "$uid_a" --dest-uid "$uid_b" --dest-gid "$gid_b" || die "A-to-B public copy failed"

run_capture respond-b "$MT" darkirc-contact respond "$TENANT_B" "$contact_b" \
  --in "$rel/offer-a.json" --out "$rel/response-b.json" --expect-peer-fingerprint "$fp_a" --json
python3 "$ANALYZER" extract --run-dir "$RUN_DIR" --output "$RUN_DIR/canaries.json" \
  --state-dir "/home/$TENANT_A/lunarwing/state/darkirc" --state-dir "/home/$TENANT_B/lunarwing/state/darkirc" \
  || die "private canary extraction failed"
fp_b="$(json_field "$RUN_DIR/commands/respond-b.stdout" public_fingerprint)"
id_b="$(json_field "$RUN_DIR/commands/respond-b.stdout" offer_id)"
python3 "$ANALYZER" verify-proc \
  --capture "$RUN_DIR/proc/prepare-a.bin" --capture "$RUN_DIR/proc/respond-b.bin" \
  --expect-cmdline lunarwing-mt-admin.sh \
  --expect-exe lunarwing-darkirc-key-helper --expect-exe darkirc \
  || die "proc sampler did not observe every key-exchange executable"
python3 "$ANALYZER" copy-public --source "$dir_b/response-b.json" --dest "$dir_a/response-b.json" \
  --source-uid "$uid_b" --dest-uid "$uid_a" --dest-gid "$gid_a" || die "B-to-A public copy failed"

run_capture complete-a "$MT" darkirc-contact complete "$TENANT_A" "$contact_a" \
  --in "$rel/response-b.json" --exchange-id "$id_a" --expect-peer-fingerprint "$fp_b" --defer-apply --json
run_capture complete-b "$MT" darkirc-contact complete "$TENANT_B" "$contact_b" \
  --in "$rel/offer-a.json" --exchange-id "$id_b" --expect-peer-fingerprint "$fp_a" --defer-apply --json
[[ "$(json_field "$RUN_DIR/commands/complete-a.stdout" state)" == "Installed" ]] || die "tenant A not Installed"
[[ "$(json_field "$RUN_DIR/commands/complete-b.stdout" state)" == "Installed" ]] || die "tenant B not Installed"

if [[ "$INIT" == systemd ]]; then
  for pair in "$TENANT_A:$uid_a" "$TENANT_B:$uid_b"; do
    tenant="${pair%%:*}"; uid="${pair##*:}"
    for unit in "lunarwing-darkirc-$tenant.service" "lunarwing-darkirc-adapter-$tenant.service"; do
      safe_unit="${unit//[^a-zA-Z0-9_.-]/_}"
      runuser -u "$tenant" -- env XDG_RUNTIME_DIR="/run/user/$uid" \
        journalctl --user --no-pager --since "@$started" -u "$unit" \
        >"$RUN_DIR/services/$safe_unit.stdout" 2>"$RUN_DIR/services/$safe_unit.stderr" \
        || die "journal capture failed for $unit"
    done
  done
else
  for tenant in "$TENANT_A" "$TENANT_B"; do
    for log in darkirc.log darkirc.err darkirc-adapter.log darkirc-adapter.err; do
      source="/home/$tenant/lunarwing/logs/$log"
      [[ -f "$source" ]] || die "OpenRC log missing: $source"
      python3 "$ANALYZER" capture-file --source "$source" --dest "$RUN_DIR/services/$tenant-$log" \
        || die "OpenRC log capture failed"
    done
  done
fi

python3 "$ANALYZER" extract --run-dir "$RUN_DIR" --output "$RUN_DIR/canaries.json" \
  --state-dir "/home/$TENANT_A/lunarwing/state/darkirc" --state-dir "/home/$TENANT_B/lunarwing/state/darkirc" \
  || die "final private canary extraction failed"
python3 "$ANALYZER" scan --canaries "$RUN_DIR/canaries.json" \
  --surface "argv-env=$RUN_DIR/proc" \
  --surface "stdout-stderr=$RUN_DIR/commands" \
  --surface "journal-log=$RUN_DIR/services" \
  --surface "public-artifacts=$dir_a,$dir_b" \
  || die "private-key canary reached observable surface"

printf 'PASS: DarkIRC H3 live validation; init=%s tenants=%s,%s raw captures deleted on exit\n' \
  "$INIT" "$TENANT_A" "$TENANT_B"
