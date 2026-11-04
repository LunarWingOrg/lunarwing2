#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
set -euo pipefail

# Hermetic harness for `tui-access` (mt-admin-lib/16-tui-access.sh; the 23
# assertions of the TUI design, section 11.4, plus regression checks for the
# review findings on listener matching, revocation, registry reads, the
# master-token fallback, privilege drops and argument parsing).
#
# Nothing here touches a real tenant, /home/<tenant>, /etc/lunarwing, sudo or
# a network peer. mt-admin is sourced and its host-facing helpers are stubbed
# AFTER sourcing (test-mt-admin-daemon-pid.sh pattern):
#  - the WI#390 `sudo -u <account>` drops run as the invoking user
#    (test-mt-admin-gateway-wait.sh), so every tenant-side and desktop-side
#    check runs for real against fixture directories owned by that user. The
#    stub also records which account each drop names, and guards on chmod,
#    rm, mkdir and the other mutating commands log any change to a desktop or
#    tenant path made outside a drop to the account that owns it;
#  - getent/id/stat answer for the fake desktop user `desk` (uid 1000) and the
#    fake tenant `alpha` (uid 1500); stat is shadowed only for desk's home, so
#    the harness also passes when CI runs it as root;
#  - curl answers scripted HTTP codes, records its argv and copies its -K
#    config; jq, sha256sum and sudo record their argv, so the test can prove
#    that no token ever reached a process argument list;
#  - /proc/net/tcp{,6} and /etc/login.defs come from fixture files through
#    TUI_PROC_NET_DIR and TUI_LOGIN_DEFS; the clock is fixed through a date stub.
# Port 20042 is 0x4E4A in /proc/net/tcp. (Design 11.4 says 4E5A, which is
# 20058; a 4E5A row held by another uid is kept as a must-not-match decoy.)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

FIX_PORT=20042
FIX_EPOCH=1793664900 # 2026-11-03T00:15:00Z
FIX_ISSUED='2026-11-03T00:15:00Z'
FIX_EXPIRES_90='2027-02-01T00:15:00Z'
# Assembled at run time so the source carries no secret-shaped literal for
# the repository's gitleaks scan.
FIX_MASTER_TOKEN="master-fixture-$(printf '%s' 0123456789 abcdef)"
tenant_root="$fixture/tenants/alpha/lunarwing"
env_dir="$tenant_root/env"
state_dir="$tenant_root/state"
registry="$state_dir/gateway-client-tokens.json"
desk_home="$fixture/home/desk"
desk_cfg="$desk_home/.config/wingtip"
desk_file="$desk_cfg/tenants/alpha.json"
calls="$fixture/calls"
OUT="$fixture/out"
ERR="$fixture/err"
RC=0
STAT_HOME_UID=1000
TUI_PROC_NET_DIR="$fixture/proc-net"
TUI_LOGIN_DEFS="$fixture/login.defs"
unset LUNARWING_CONTAINER_RUNTIME

mkdir -p "$env_dir" "$state_dir" "$desk_home" "$calls" "$TUI_PROC_NET_DIR" \
  "$fixture/home/alpha" "$fixture/home/svc" "$fixture/elsewhere"
chmod 700 "$env_dir" "$state_dir" "$desk_home"
printf '# fixture login.defs\nUID_MIN                  1000\nUID_MAX                 60000\n' >"$TUI_LOGIN_DEFS"

# ── Stubs (after sourcing) ───────────────────────────────────────────────────

require_root() { :; }
openrc_dispatch_preflight() { return 0; }
ports_registry_require_readonly() { :; }
# Mirrors the real helper's errexit contract: the callback runs with errexit
# off, so a step that forgets its status check fails the assertions here.
with_tenant_lifecycle_lock() {
  local rc
  shift
  set +e
  "$@"
  rc=$?
  set -e
  return "$rc"
}
tenant_exists_in_registry() { [[ "$1" == alpha ]]; }
ports_get() {
  [[ "$1" == alpha && "$2" == gateway ]] || return 1
  printf '%s' "$FIX_PORT"
}
tenant_lw_root() { printf '%s/tenants/%s/lunarwing' "$fixture" "$1"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
tenant_state_dir() { printf '%s/state' "$(tenant_lw_root "$1")"; }
getent() {
  [[ "${1:-}" == passwd && $# -eq 2 ]] || return 2
  case "$2" in
    desk) printf 'desk:x:1000:1000:Desk User:%s:/bin/bash\n' "$desk_home" ;;
    root) printf 'root:x:0:0:root:/root:/bin/bash\n' ;;
    alpha) printf 'alpha:x:1500:1500::%s/home/alpha:/bin/bash\n' "$fixture" ;;
    svc) printf 'svc:x:999:999::%s/home/svc:/usr/sbin/nologin\n' "$fixture" ;;
    *) return 2 ;;
  esac
}
id() {
  if [[ "${1:-}" == -u && ( "${2:-}" == alpha || ( "${2:-}" == -- && "${3:-}" == alpha ) ) ]]; then
    printf '1500\n'
    return 0
  fi
  command id "$@"
}
stat() {
  if [[ $# -eq 3 && "$1" == -c && "$2" == %u && "$3" == "$desk_home" ]]; then
    printf '%s\n' "$STAT_HOME_UID"
    return 0
  fi
  command stat "$@"
}
date() {
  if [[ "$*" == "-u +%s" ]]; then
    printf '%s\n' "$FIX_EPOCH"
    return 0
  fi
  command date "$@"
}
sleep() { printf '%s\n' "$*" >>"$calls/sleep.argv"; }
# TEST_DROP_AS names the account of the drop in progress, so the guards below
# can tell a dropped chmod/rm/mkdir from one the verb ran itself as root.
TEST_DROP_AS=""
sudo() {
  [[ "${1:-}" == -u && $# -ge 3 ]] || return 1
  printf '%s\n' "$*" >>"$calls/sudo.argv"
  local outer="$TEST_DROP_AS" rc=0
  TEST_DROP_AS="$2"
  shift 2
  "$@" || rc=$?
  TEST_DROP_AS="$outer"
  return "$rc"
}
# WI#390/WI#446: root never changes a desktop or tenant path itself (chmod
# follows symlinks, and a path checked as root can be swapped afterwards).
# While the verb runs (TEST_IN_VERB), a mutating command on a path under the
# fixture desktop home or tenant root must run inside a drop to the account
# owning that path; every other one is logged to $undropped.
undropped="$fixture/undropped.log"
: >"$undropped"
guard_mutation() {  # <command> <arguments...>
  local arg owner
  if [[ "${TEST_IN_VERB:-}" == true ]]; then
    for arg in "${@:2}"; do
      case "$arg" in
        "$desk_home" | "$desk_home"/*) owner=desk ;;
        "$tenant_root" | "$tenant_root"/*) owner=alpha ;;
        *) continue ;;
      esac
      [[ "$TEST_DROP_AS" == "$owner" ]] \
        || printf '%s %s outside a drop to %s\n' "$1" "$arg" "$owner" >>"$undropped"
    done
  fi
  command "$@"
}
chmod() { guard_mutation chmod "$@"; }
chown() { guard_mutation chown "$@"; }
rm() { guard_mutation rm "$@"; }
rmdir() { guard_mutation rmdir "$@"; }
mkdir() { guard_mutation mkdir "$@"; }
mv() { guard_mutation mv "$@"; }
ln() { guard_mutation ln "$@"; }
cp() { guard_mutation cp "$@"; }
touch() { guard_mutation touch "$@"; }
install() { guard_mutation install "$@"; }
truncate() { guard_mutation truncate "$@"; }
# FAKE_BYTE_ORDER=big makes the listener check's byte-order probe answer as
# on a big-endian host.
FAKE_BYTE_ORDER=""
od() {
  if [[ "$FAKE_BYTE_ORDER" == big && "$*" == "-An -tx2" ]]; then
    cat >/dev/null
    printf ' 0100\n'
    return 0
  fi
  command od "$@"
}
jq() {
  printf '%s\n' "$*" >>"$calls/jq.argv"
  command jq "$@"
}
sha256sum() {
  printf '%s\n' "$*" >>"$calls/sha256sum.argv"
  command sha256sum "$@"
}
# Scripted gateway: $fixture/health-code holds the /api/health answer;
# $fixture/status-codes one /api/gateway/status answer per line (the last
# line repeats); $fixture/capabilities-code and capabilities-body answer
# /api/chat/capabilities (the body lands in the -o file). 000 means nothing
# answered.
curl() {
  local arg prev="" url="" cfg="" out="" n code
  printf '%s\n' "$*" >>"$calls/curl.argv"
  n="$(wc -l <"$calls/curl.argv")"
  for arg in "$@"; do
    [[ "$prev" != -K ]] || cfg="$arg"
    [[ "$prev" != -o ]] || out="$arg"
    prev="$arg"
    url="$arg"
  done
  if [[ -n "$cfg" ]]; then
    cat -- "$cfg" >"$calls/curl-config.$n"
  fi
  case "$url" in
    */api/health) code="$(<"$fixture/health-code")" ;;
    */api/chat/capabilities)
      code="$(<"$fixture/capabilities-code")"
      if [[ -n "$out" && "$out" != /dev/null ]]; then
        cat -- "$fixture/capabilities-body" >"$out"
      fi
      ;;
    */api/gateway/status)
      code="$(head -n 1 "$fixture/status-codes")"
      if (($(wc -l <"$fixture/status-codes") > 1)); then
        tail -n +2 "$fixture/status-codes" >"$fixture/status-codes.next"
        mv -- "$fixture/status-codes.next" "$fixture/status-codes"
      fi
      ;;
    *) code=000 ;;
  esac
  printf '%s' "$code"
  [[ "$code" != 000 ]] || return 7
}

# ── Fixture helpers ──────────────────────────────────────────────────────────

write_env() {  # [KEY=VALUE...] overrides of the default tenant env
  local -A kv=(
    [LUNARWING_BASE_DIR]="$state_dir"
    [GATEWAY_ENABLED]=true
    [GATEWAY_HOST]=0.0.0.0
    [GATEWAY_PORT]="$FIX_PORT"
    [GATEWAY_AUTH_TOKEN]="$FIX_MASTER_TOKEN"
  )
  local pair key
  for pair in "$@"; do
    kv[${pair%%=*}]="${pair#*=}"
  done
  : >"$env_dir/lunarwing.env"
  for key in LUNARWING_BASE_DIR GATEWAY_ENABLED GATEWAY_HOST GATEWAY_PORT GATEWAY_AUTH_TOKEN GATEWAY_CLIENT_TOKENS_FILE; do
    if [[ -v "kv[$key]" ]]; then
      printf '%s=%s\n' "$key" "${kv[$key]}" >>"$env_dir/lunarwing.env"
    fi
  done
  chmod 600 "$env_dir/lunarwing.env"
}

write_proc_net() {  # <uid holding 0.0.0.0:20042> [extra tcp6 line]
  {
    printf '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n'
    printf '   0: 00000000:4E4A 00000000:0000 0A 00000000:00000000 00:00000000 00000000  %s        0 31337 1 0000000000000000 100 0 0 10 0\n' "$1"
    printf '   1: 00000000:4E5A 00000000:0000 0A 00000000:00000000 00:00000000 00000000  4242        0 31338 1 0000000000000000 100 0 0 10 0\n'
    printf '   2: 0100007F:4E4A 0100007F:A1B2 01 00000000:00000000 00:00000000 00000000  4243        0 31339 1 0000000000000000 20 4 30 10 -1\n'
  } >"$TUI_PROC_NET_DIR/tcp"
  {
    printf '  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n'
    printf '   0: 00000000000000000000000001000000:4E5B 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  4244        0 31340 1 0000000000000000 100 0 0 10 0\n'
    if [[ -n "${2:-}" ]]; then printf '%s\n' "$2"; fi
  } >"$TUI_PROC_NET_DIR/tcp6"
}

gateway_answers() {  # <health code> <status code...>
  printf '%s\n' "$1" >"$fixture/health-code"
  shift
  printf '%s\n' "$@" >"$fixture/status-codes"
}

gateway_capabilities() {  # <code> [body]: the /api/chat/capabilities answer
  printf '%s\n' "$1" >"$fixture/capabilities-code"
  printf '%s\n' "${2:-}" >"$fixture/capabilities-body"
}
CAPS_WITH_CLIENT_TOKENS='{"api_version":1,"features":["status","attachments","client_tokens"],"credential":"master"}'
CAPS_WITHOUT_CLIENT_TOKENS='{"api_version":1,"features":["status","attachments"],"credential":"master"}'
# Default: a daemon that authenticates client tokens, so every grant below
# passes the UZ-1 capability check unless a scenario says otherwise.
gateway_capabilities 200 "$CAPS_WITH_CLIENT_TOKENS"

seed_registry() {  # <exact file content>
  printf '%s\n' "$1" >"$registry"
  chmod 600 "$registry"
}

clean_grant_state() {
  rm -f -- "$desk_file"
  seed_registry '{"version":1,"tokens":[]}'
}

reset_calls() {
  rm -rf "$calls"
  mkdir -p "$calls"
  : >"$calls/curl.argv"
  : >"$calls/jq.argv"
  : >"$calls/sha256sum.argv"
  : >"$calls/sudo.argv"
}

run_tui() {  # <tui_access_command args...>; sets RC, writes $OUT and $ERR
  reset_calls
  set +e
  (
    set -e
    TEST_IN_VERB=true
    tui_access_command "$@"
  ) >"$OUT" 2>"$ERR"
  RC=$?
  set -e
}

# run_tui for a registry that may block a reader (a FIFO): gives up after
# <seconds> instead of hanging the harness, then releases any reader still
# blocked opening the FIFO (an O_RDWR open never blocks on Linux).
run_tui_bounded() {  # <seconds> <tui_access_command args...>
  local limit="$1" pid ticks=0 fd
  shift
  reset_calls
  set +e
  (
    set -e
    TEST_IN_VERB=true
    tui_access_command "$@"
  ) >"$OUT" 2>"$ERR" &
  pid=$!
  while kill -0 "$pid" 2>/dev/null && ((ticks < limit * 10)); do
    command sleep 0.1
    ticks=$((ticks + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    kill -KILL "$pid" 2>/dev/null
    printf 'harness: tui-access still running after %s s, killed\n' "$limit" >>"$ERR"
  fi
  if [[ -p "$registry" ]]; then
    exec {fd}<>"$registry"
    exec {fd}>&-
  fi
  wait "$pid"
  RC=$?
  set -e
}

# Privilege drops recorded by the sudo stub during the last run.
drop_ran() {  # <account> <exact command line>
  grep -qxF -- "-u $1 $2" "$calls/sudo.argv"
}
drop_began() {  # <account> <command prefix>
  awk -v p="-u $1 $2" 'index($0, p) == 1 { found = 1 } END { exit !found }' "$calls/sudo.argv"
}

file_sum() {  # <file>: content checksum, or "absent"
  if [[ -e "$1" || -L "$1" ]]; then
    command sha256sum <"$1" | cut -d' ' -f1
  else
    printf 'absent'
  fi
}

mode_of() { command stat -c %a -- "$1"; }

# The secret travels on a pipe into grep (-f), never through argv.
contains_secret() {  # <file> <secret>
  grep -qF -f <(printf '%s\n' "$2") -- "$1"
}

desk_json() { command jq -r "$1" "$desk_file" 2>/dev/null || true; }

digest_of() { printf '%s' "$1" | command sha256sum | cut -d' ' -f1; }

status_calls() { grep -c '/api/gateway/status' "$calls/curl.argv" || true; }

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}
# Failure dumps redact anything token-shaped, so even a leak found by an
# assertion is not echoed into the CI log.
redacted_dump() {  # <prefix> <file>
  sed -E -e 's/[0-9a-f]{64}/<hex64>/g' -e 's/master-fixture-[0-9a-f]+/<master>/g' \
    -e "s/^/      $1: /" -- "$2" 2>/dev/null || true
}
check() {  # <label> <command...>
  local label="$1"
  shift
  if "$@"; then
    pass "$label"
  else
    fail "$label"
    printf '      rc=%s\n' "$RC"
    redacted_dump out "$OUT"
    redacted_dump err "$ERR"
  fi
}

refused_with() {  # <pattern>: the last run exited 1 and said <pattern> on stderr
  ((RC == 1)) && grep -qF -- "$1" "$ERR"
}

# ── Scenario A: first grant in client-token mode ─────────────────────────────

write_env
write_proc_net 1500
seed_registry '{"version":1,"tokens":[{"label":"tui:other","sha256":"'"$(printf '%064d' 7)"'","created_at":"2026-10-01T00:00:00Z"}]}'
# The first status probe lands before the daemon's 2 s reload.
gateway_answers 200 401 200
run_tui alpha --for-user desk --expires-in-days 90
TOKEN_A="$(desk_json .token)"
DIGEST_A="$(digest_of "$TOKEN_A")"

assert_1() {
  ((RC == 0)) && [[ -f "$desk_file" && ! -L "$desk_file" ]] && [[ "$(mode_of "$desk_file")" == 600 ]] \
    && [[ "$(desk_json .url)" == "http://127.0.0.1:$FIX_PORT" ]] \
    && [[ "$TOKEN_A" =~ ^[0-9a-f]{64}$ ]] \
    && [[ "$(desk_json '[.schema, .version, .tenant, .transport, .token_kind, .label, .issued_at, .expires_at] | map(tostring) | join(" ")')" \
      == "wingtip/tenant-access 1 alpha gateway client tui:desk $FIX_ISSUED $FIX_EXPIRES_90" ]]
}
check "1. grant writes 0600 desktop config with http://127.0.0.1:20042 and a 64-hex token" assert_1

assert_2() {
  [[ "$(mode_of "$desk_cfg")" == 700 && "$(mode_of "$desk_cfg/tenants")" == 700 ]]
}
check "2. desktop config directories are 0700" assert_2

assert_3() {
  [[ "$(mode_of "$registry")" == 600 ]] \
    && [[ "$(command jq -r --arg l tui:desk '[.tokens[] | select(.label == $l)] | length' "$registry")" == 1 ]] \
    && [[ "$(command jq -r --arg l tui:desk '.tokens[] | select(.label == $l) | [.sha256, .created_at, .expires_at] | join(" ")' "$registry")" \
      == "$DIGEST_A $FIX_ISSUED $FIX_EXPIRES_90" ]] \
    && [[ "$(command jq -r '[.tokens[].label] | join(",")' "$registry")" == "tui:other,tui:desk" ]] \
    && ! contains_secret "$registry" "$TOKEN_A"
}
check "3. tenant registry holds sha256(token) under label tui:desk and no plaintext token" assert_3

assert_4() {
  ! contains_secret "$OUT" "$TOKEN_A" && ! contains_secret "$ERR" "$TOKEN_A" \
    && ! contains_secret "$OUT" "$DIGEST_A" && grep -q "granted tenant 'alpha' to user 'desk'" "$OUT"
}
check "4. token absent from stdout and stderr of the grant" assert_4

assert_5() {
  local f
  # Each stub must have run, or its absence check would prove nothing.
  [[ -s "$calls/curl.argv" && -s "$calls/jq.argv" && -s "$calls/sha256sum.argv" && -s "$calls/sudo.argv" ]] || return 1
  for f in curl jq sha256sum sudo; do
    ! contains_secret "$calls/$f.argv" "$TOKEN_A" || return 1
  done
}
check "5. token absent from every recorded argv (curl, jq, sha256sum stubs)" assert_5

assert_6() {
  local cfg found=false
  for cfg in "$calls"/curl-config.*; do
    [[ -e "$cfg" ]] || continue
    grep -qxF -f <(printf 'header = "Authorization: Bearer %s"\n' "$TOKEN_A") -- "$cfg" && found=true
  done
  [[ "$found" == true ]] \
    && grep -q -- "-K /dev/fd/[0-9]* http://127.0.0.1:$FIX_PORT/api/gateway/status" "$calls/curl.argv" \
    && grep -q 'verify:  ok (GET /api/gateway/status -> 200)' "$OUT" \
    && [[ "$(status_calls)" == 2 && "$(cat "$calls/sleep.argv")" == 2 ]]
}
check "6. verify probe sends Authorization: Bearer via curl -K config" assert_6

assert_23() {
  [[ "$(desk_json '.tenant_uid | type')" == number && "$(desk_json .tenant_uid)" == 1500 ]] \
    && grep -q 'owner:   port 20042 is held by uid 1500 (alpha); recorded as tenant_uid' "$OUT"
}
check "23. desktop config records tenant_uid 1500 from id -u alpha" assert_23

assert_grant_drops() {
  drop_ran desk "mkdir -p -- $desk_cfg/tenants" \
    && drop_ran desk "chmod 700 -- $desk_cfg" \
    && drop_ran desk "chmod 700 -- $desk_cfg/tenants" \
    && drop_began desk "env -i PATH=/usr/bin:/bin TARGET_PATH=$desk_file TARGET_MODE=600 PARENT_POLICY=private " \
    && drop_began alpha "env -i PATH=/usr/bin:/bin TARGET_PATH=$registry MAX_BYTES=65536 timeout " \
    && drop_began alpha "jq -s -r " \
    && drop_began alpha "env -i PATH=/usr/bin:/bin TARGET_PATH=$registry TARGET_MODE=600 PARENT_POLICY=owned-parent " \
    && [[ ! -s "$undropped" ]]
}
check "drops: the grant creates, tightens and writes the desktop side as desk and reads and writes the registry as alpha" assert_grant_drops

# ── Scenario B: --list after the grant ───────────────────────────────────────

run_tui alpha --list
assert_18() {
  ((RC == 0)) \
    && grep -Eq "^  tui:desk +$FIX_ISSUED +$FIX_EXPIRES_90\$" "$OUT" \
    && grep -Eq '^  tui:other +2026-10-01T00:00:00Z +-$' "$OUT" \
    && ! contains_secret "$OUT" "$DIGEST_A" && ! contains_secret "$OUT" "$TOKEN_A" \
    && ! contains_secret "$ERR" "$DIGEST_A" && ! grep -q "$(printf '%064d' 7)" "$OUT"
}
check "18. --list prints validated labels and dates, never digests or tokens" assert_18

# ── Scenario C: host mapping (no probe) ──────────────────────────────────────

url_for_host() {  # <GATEWAY_HOST> [extra tui-access args...]
  local host="$1"
  shift
  write_env "GATEWAY_HOST=$host"
  run_tui alpha --for-user desk --no-verify "$@"
  ((RC == 0)) || return 1
  desk_json .url
}
assert_7() {
  [[ "$(url_for_host 0.0.0.0)" == "http://127.0.0.1:$FIX_PORT" ]] \
    && [[ "$(url_for_host 100.88.107.159)" == "http://100.88.107.159:$FIX_PORT" ]] \
    && grep -q 'is not a loopback URL' "$ERR" \
    && [[ "$(url_for_host ::1)" == "http://[::1]:$FIX_PORT" ]] \
    && [[ "$(url_for_host 0.0.0.0 --url-host 10.20.30.40)" == "http://10.20.30.40:$FIX_PORT" ]] \
    && [[ ! -s "$calls/curl.argv" ]] && grep -q 'verify:  skipped (--no-verify)' "$OUT"
}
check "7. GATEWAY_HOST=0.0.0.0 -> 127.0.0.1; 100.88.107.159 kept; ::1 -> [::1]; --url-host overrides" assert_7
write_env

# ── Scenario D: rotation ─────────────────────────────────────────────────────

gateway_answers 200 200
run_tui alpha --for-user desk
TOKEN_A="$(desk_json .token)"
DIGEST_A="$(digest_of "$TOKEN_A")"
gateway_answers 200 200
run_tui alpha --for-user desk
TOKEN_B="$(desk_json .token)"
DIGEST_B="$(digest_of "$TOKEN_B")"
assert_8() {
  ((RC == 0)) && [[ "$TOKEN_B" =~ ^[0-9a-f]{64}$ && "$TOKEN_B" != "$TOKEN_A" ]] \
    && [[ "$(command jq -r '[.tokens[] | select(.label == "tui:desk")] | length' "$registry")" == 1 ]] \
    && [[ "$(command jq -r '.tokens[] | select(.label == "tui:desk") | .sha256' "$registry")" == "$DIGEST_B" ]] \
    && ! grep -qF "$DIGEST_A" "$registry" \
    && [[ "$(desk_json 'has("expires_at")')" == false ]] \
    && [[ "$(command jq -r '[.tokens[].label] | join(",")' "$registry")" == "tui:other,tui:desk" ]]
}
check "8. re-grant rotates: new token, exactly one tui:desk entry, previous digest gone" assert_8

# ── Scenario E: revoke ───────────────────────────────────────────────────────

run_tui alpha --for-user desk --revoke
assert_revoke_drops() {
  drop_ran desk "rm -f -- $desk_file" && ! drop_began desk "jq " \
    && drop_began alpha "env -i PATH=/usr/bin:/bin TARGET_PATH=$registry MAX_BYTES=65536 timeout " \
    && drop_began alpha "env -i PATH=/usr/bin:/bin TARGET_PATH=$registry TARGET_MODE=600 PARENT_POLICY=owned-parent " \
    && [[ ! -s "$undropped" ]]
}
check "drops: revoke removes the desktop file as desk without reading it and rewrites the registry as alpha" assert_revoke_drops
assert_9() {
  local first_rc="$RC" first_out sum_after
  first_out="$(cat "$OUT")"
  sum_after="$(file_sum "$registry")"
  ((first_rc == 0)) && [[ ! -e "$desk_file" ]] \
    && [[ "$(command jq -r '[.tokens[].label] | join(",")' "$registry")" == "tui:other" ]] \
    && [[ "$first_out" == *"removed 'tui:desk'"* ]] || return 1
  run_tui alpha --for-user desk --revoke
  ((RC == 0)) && grep -q "nothing to revoke for user 'desk' on tenant 'alpha'" "$OUT" \
    && [[ "$(file_sum "$registry")" == "$sum_after" ]]
}
check "9. revoke removes registry entry and desktop file; second revoke is a no-op" assert_9

# ── Scenario F: refused desktop users ────────────────────────────────────────

assert_10() {
  local before
  before="$(file_sum "$registry")"
  run_tui alpha --for-user root
  refused_with "uid 0" || return 1
  run_tui alpha --for-user alpha
  refused_with "it is a registered tenant" || return 1
  run_tui alpha --for-user nosuchuser
  refused_with "unknown user 'nosuchuser'" || return 1
  run_tui alpha --for-user svc
  refused_with "uid 999 is below UID_MIN 1000" || return 1
  [[ "$(file_sum "$registry")" == "$before" && ! -e "$fixture/home/svc/.config" \
    && ! -e "$fixture/home/alpha/.config" && ! -e "$desk_file" ]]
}
check "10. refuses --for-user root, a tenant name, an unknown user, uid below UID_MIN" assert_10

# ── Scenario G: unsafe desktop paths ─────────────────────────────────────────

assert_11() {
  local before ok=true
  before="$(file_sum "$registry")"
  mv -- "$desk_home/.config" "$desk_home/.config.real"
  ln -s -- "$fixture/elsewhere" "$desk_home/.config"
  gateway_answers 200 200
  run_tui alpha --for-user desk
  refused_with "unsafe tenant directory path" || ok=false
  [[ -z "$(ls -A "$fixture/elsewhere")" && "$(file_sum "$registry")" == "$before" ]] || ok=false
  rm -- "$desk_home/.config"
  mv -- "$desk_home/.config.real" "$desk_home/.config"
  [[ "$ok" == true ]]
}
check "11. refuses a symlinked ~/.config component" assert_11

assert_12() {
  local before ok=true
  before="$(file_sum "$registry")"
  printf 'victim\n' >"$fixture/victim"
  ln -s -- "$fixture/victim" "$desk_file"
  gateway_answers 200 200
  run_tui alpha --for-user desk
  refused_with "exit 73" || ok=false
  [[ -L "$desk_file" && "$(cat "$fixture/victim")" == victim ]] || ok=false
  # The UZ-1 capability check may run first; the token probe must not.
  [[ "$(file_sum "$registry")" == "$before" && "$(status_calls)" == 0 ]] || ok=false
  rm -- "$desk_file"
  [[ "$ok" == true ]]
}
check "12. refuses when the destination file is a symlink (writer exit 73)" assert_12

# ── Scenario H: master-token mode ────────────────────────────────────────────

gateway_answers 200 200
before_master="$(file_sum "$registry")"
run_tui alpha --for-user desk --use-master-token
assert_13() {
  local f cfg_found=false cfg
  ((RC == 0)) && [[ "$(mode_of "$desk_file")" == 600 ]] \
    && [[ "$(desk_json .token_kind)" == master && "$(desk_json .token)" == "$FIX_MASTER_TOKEN" ]] \
    && [[ "$(desk_json .tenant_uid)" == 1500 ]] \
    && [[ "$(file_sum "$registry")" == "$before_master" ]] \
    && grep -q 'WARNING: --use-master-token copies the GATEWAY_AUTH_TOKEN' "$ERR" || return 1
  ! contains_secret "$OUT" "$FIX_MASTER_TOKEN" && ! contains_secret "$ERR" "$FIX_MASTER_TOKEN" || return 1
  for f in curl jq sha256sum sudo; do
    ! contains_secret "$calls/$f.argv" "$FIX_MASTER_TOKEN" || return 1
  done
  for cfg in "$calls"/curl-config.*; do
    [[ -e "$cfg" ]] || continue
    contains_secret "$cfg" "Authorization: Bearer $FIX_MASTER_TOKEN" && cfg_found=true
  done
  [[ "$cfg_found" == true ]]
}
check "13. --use-master-token writes token_kind master with GATEWAY_AUTH_TOKEN, leaves registry untouched, prints warning" assert_13

run_tui alpha --for-user desk --revoke
check "revoke of a master-token grant warns that the master token stays valid" \
  eval '((RC == 0)) && [[ ! -e "$desk_file" ]] && grep -q "held the master token" "$ERR"'

# ── Scenario I: the daemon rejects the token ─────────────────────────────────

gateway_answers 200 401
run_tui alpha --for-user desk
assert_14() {
  ((RC == 2)) && grep -q "re-run the grant" "$ERR" && grep -q -- "--use-master-token" "$ERR" \
    && [[ -f "$desk_file" ]] \
    && [[ "$(command jq -r '.tokens[] | select(.label == "tui:desk") | .sha256' "$registry")" == "$(digest_of "$(desk_json .token)")" ]] \
    && [[ "$(status_calls)" == 4 && "$(wc -l <"$calls/sleep.argv")" == 3 ]] \
    && ! contains_secret "$ERR" "$(desk_json .token)"
}
check "14. 401 on every verify attempt -> exit 2 with re-run/--use-master-token guidance; files kept" assert_14

# The documented fallback for today's daemons: the exit-2 grant above left
# its digest under tui:desk; the master-token re-grant must not leave it live.
gateway_answers 200 200
run_tui alpha --for-user desk --use-master-token
assert_master_fallback() {
  ((RC == 0)) && [[ "$(desk_json .token_kind)" == master ]] \
    && [[ "$(command jq -r '[.tokens[].label] | join(",")' "$registry")" == "tui:other" ]] \
    && grep -qF "removed: earlier client token 'tui:desk' from $registry" "$OUT" \
    && ! contains_secret "$OUT" "$FIX_MASTER_TOKEN" && ! contains_secret "$ERR" "$FIX_MASTER_TOKEN"
}
check "--use-master-token after an exit-2 client grant removes the earlier tui:desk digest" assert_master_fallback

# ── Scenario I2: the daemon does not authenticate client tokens (UZ-1) ───────

gate_refuses() {  # <capabilities code> <capabilities body>: a client-mode grant is refused before anything is written
  local before
  clean_grant_state
  before="$(file_sum "$registry")"
  gateway_capabilities "$1" "$2"
  gateway_answers 200 200
  run_tui alpha --for-user desk
  refused_with "does not authenticate client tokens" && grep -q -- "--use-master-token" "$ERR" \
    && grep -q "rotate-gateway-token alpha" "$ERR" \
    && [[ ! -e "$desk_file" && "$(file_sum "$registry")" == "$before" ]] \
    && [[ "$(status_calls)" == 0 ]] \
    && grep -q '/api/chat/capabilities' "$calls/curl.argv" \
    && ! grep -q 'Authorization' "$calls/curl.argv"
}
check "UZ-1: capabilities without client_tokens -> client grant refused, nothing written, no token probe" \
  gate_refuses 200 "$CAPS_WITHOUT_CLIENT_TOKENS"
check "UZ-1: no capabilities route (404) -> client grant refused the same way" \
  gate_refuses 404 ''
assert_gate_master_cfg() {
  local cfg found=false
  for cfg in "$calls"/curl-config.*; do
    [[ -e "$cfg" ]] || continue
    contains_secret "$cfg" "Authorization: Bearer $FIX_MASTER_TOKEN" && found=true
  done
  [[ "$found" == true ]] && ! contains_secret "$calls/curl.argv" "$FIX_MASTER_TOKEN" \
    && ! contains_secret "$OUT" "$FIX_MASTER_TOKEN" && ! contains_secret "$ERR" "$FIX_MASTER_TOKEN"
}
check "UZ-1: the capability check sends the master token only through a curl -K config" assert_gate_master_cfg
gateway_capabilities 200 "$CAPS_WITHOUT_CLIENT_TOKENS"
gateway_answers 200 200
run_tui alpha --for-user desk --use-master-token
check "UZ-1: --use-master-token still works against such a daemon and is called a stopgap" \
  eval '((RC == 0)) && [[ "$(desk_json .token_kind)" == master ]] && grep -q "stopgap" "$ERR" && grep -q "rotate-gateway-token alpha" "$ERR"'
clean_grant_state
gateway_capabilities 200 "$CAPS_WITH_CLIENT_TOKENS"
gateway_answers 000
run_tui alpha --for-user desk
check "UZ-1: an unreachable gateway leaves support undetermined: files written, reason printed, exit 0" \
  eval '((RC == 0)) && [[ -f "$desk_file" ]] && grep -q "support: not checked (gateway not reachable now" "$OUT" && [[ "$(status_calls)" == 0 ]]'
clean_grant_state
gateway_capabilities 200 "$CAPS_WITHOUT_CLIENT_TOKENS"
gateway_answers 200 200
run_tui alpha --for-user desk --no-verify
check "UZ-1: --no-verify skips the capability check (warned) and the probe, and writes the files" \
  eval '((RC == 0)) && [[ -f "$desk_file" ]] && grep -q "not checking whether the daemon" "$ERR" && [[ ! -s "$calls/curl.argv" ]]'
gateway_capabilities 200 "$CAPS_WITH_CLIENT_TOKENS"
clean_grant_state

# ── Scenario J: tenant env policy ────────────────────────────────────────────

refuses_without_writing() {  # <pattern> <env overrides...>
  local pattern="$1" before
  shift
  clean_grant_state
  before="$(file_sum "$registry")"
  write_env "$@"
  gateway_answers 200 200
  run_tui alpha --for-user desk
  refused_with "$pattern" && [[ ! -e "$desk_file" && "$(file_sum "$registry")" == "$before" ]] \
    && [[ ! -s "$calls/curl.argv" ]]
}
check "15. GATEWAY_ENABLED=false -> refuses" \
  refuses_without_writing "is disabled (GATEWAY_ENABLED=false)" GATEWAY_ENABLED=false
check "16. LUNARWING_BASE_DIR != tenant_state_dir -> refuses" \
  refuses_without_writing "LUNARWING_BASE_DIR of tenant 'alpha'" "LUNARWING_BASE_DIR=$fixture/elsewhere"
assert_17() {
  refuses_without_writing "GATEWAY_CLIENT_TOKENS_FILE=none" GATEWAY_CLIENT_TOKENS_FILE=none \
    && grep -q -- "--use-master-token" "$ERR"
}
check "17. GATEWAY_CLIENT_TOKENS_FILE=none -> refuses client mode, suggests --use-master-token" assert_17
check "22. explicit GATEWAY_CLIENT_TOKENS_FILE outside state/ is refused" \
  refuses_without_writing "GATEWAY_CLIENT_TOKENS_FILE must be unset, none, or $registry" \
  "GATEWAY_CLIENT_TOKENS_FILE=$fixture/elsewhere/gateway-client-tokens.json"
write_env

assert_revoke_policy_refused() {
  local before
  gateway_answers 200 200
  run_tui alpha --for-user desk --no-verify
  ((RC == 0)) && [[ -f "$desk_file" ]] || return 1
  before="$(file_sum "$registry")"
  write_env "LUNARWING_BASE_DIR=$fixture/elsewhere"
  run_tui alpha --for-user desk --revoke
  write_env
  ((RC == 0)) && [[ ! -e "$desk_file" && "$(file_sum "$registry")" == "$before" ]] \
    && grep -q 'the tenant-side registry was not touched' "$ERR" && grep -q 'registry: not checked' "$OUT"
}
check "revoke under a refused registry path still removes the desktop file and says so" assert_revoke_policy_refused

assert_revoke_unknown_user() {
  seed_registry '{"version":1,"tokens":[{"label":"tui:ghost","sha256":"'"$(printf '%064d' 9)"'"},{"label":"tui:other","sha256":"'"$(printf '%064d' 8)"'"}]}'
  run_tui alpha --for-user ghost --revoke
  ((RC == 0)) && [[ "$(command jq -r '[.tokens[].label] | join(",")' "$registry")" == "tui:other" ]] \
    && grep -q "has no passwd entry" "$ERR" && grep -q "config:   not checked (no passwd entry for 'ghost')" "$OUT"
}
check "revoke for a deleted account still removes its tenant-side entry" assert_revoke_unknown_user

# Design 9.4: --revoke is the per-user emergency cut-off, so the state of the
# desktop home must not stop the tenant-side removal (the grant still refuses).
revoke_with_unusable_home() {  # <foreign-owned|missing>
  local how="$1" ok=true
  clean_grant_state
  gateway_answers 200 200
  run_tui alpha --for-user desk --no-verify
  ((RC == 0)) && [[ -f "$desk_file" ]] || return 1
  case "$how" in
    foreign-owned) STAT_HOME_UID=0 ;;
    missing) mv -- "$desk_home" "$fixture/home/desk.away" ;;
  esac
  run_tui alpha --for-user desk --no-verify
  refused_with "refusing --for-user 'desk': home $desk_home" || ok=false
  run_tui alpha --for-user desk --revoke
  { ((RC == 0)) && [[ "$(command jq -r '[.tokens[].label] | join(",")' "$registry")" == "" ]] \
    && grep -qF "revoked tenant 'alpha' for user 'desk'" "$OUT" \
    && grep -qF "registry: removed 'tui:desk'" "$OUT" \
    && grep -qF "config:   not checked (home $desk_home" "$OUT" \
    && grep -qF "remove ~desk/.config/wingtip/tenants/alpha.json by hand" "$OUT" \
    && grep -qF "its desktop config is not checked" "$ERR" \
    && grep -qF "should a desktop config of 'desk' hold the master token" "$ERR"; } || ok=false
  STAT_HOME_UID=1000
  if [[ "$how" == missing ]]; then mv -- "$fixture/home/desk.away" "$desk_home"; fi
  rm -f -- "$desk_file"
  [[ "$ok" == true ]]
}
check "revoke removes the tenant-side entry although the desktop home is owned by another uid" \
  revoke_with_unusable_home foreign-owned
check "revoke removes the tenant-side entry although the desktop home is missing" \
  revoke_with_unusable_home missing

assert_revoke_refused_registry() {
  local before
  clean_grant_state
  gateway_answers 200 200
  run_tui alpha --for-user desk --use-master-token --no-verify
  ((RC == 0)) && [[ "$(desk_json .token_kind)" == master ]] || return 1
  seed_registry '{"version":2}'
  before="$(file_sum "$registry")"
  run_tui alpha --for-user desk --revoke
  ((RC == 1)) && [[ ! -e "$desk_file" && "$(file_sum "$registry")" == "$before" ]] \
    && grep -qF 'its version is not 1' "$ERR" \
    && grep -qF "the tenant-side 'tui:desk' entry of tenant 'alpha' may still be registered" "$ERR" \
    && grep -qF 'held the master token' "$ERR" \
    && grep -qF "config:   removed $desk_file" "$OUT" \
    && grep -qF 'registry: not changed' "$OUT"
}
check "revoke with a malformed registry still removes the desktop file, then exits 1" assert_revoke_refused_registry

# ── Scenario K: registry shape ───────────────────────────────────────────────

refuses_registry() {  # <content> <pattern>
  local before
  rm -f -- "$desk_file"
  seed_registry "$1"
  before="$(file_sum "$registry")"
  gateway_answers 200 200
  run_tui alpha --for-user desk
  refused_with "$2" && [[ "$(file_sum "$registry")" == "$before" && ! -e "$desk_file" ]]
}
assert_19() {
  refuses_registry '{"version":2,"tokens":[]}' "its version is not 1" \
    && refuses_registry '{"version":1,"tokens":{"label":"tui:x"}}' "its tokens field is not an array" \
    && refuses_registry '{"version":1,"tokens":[]} {"version":1,"tokens":[]}' "exactly one JSON document"
}
check "19. registry file with version 2 or non-array tokens -> refuses without rewriting it" assert_19

# ── Scenario K2: registry files the daemon would refuse to load ──────────────

pad_registry() {  # <file size in bytes, trailing newline included>
  local prefix suffix='"}]}' fill
  prefix='{"version":1,"tokens":[{"label":"tui:pad","sha256":"'"$(printf '%064d' 5)"'","pad":"'
  fill=$(($1 - ${#prefix} - ${#suffix} - 1))
  { printf '%s' "$prefix"; printf '%*s' "$fill" '' | tr ' ' x; printf '%s\n' "$suffix"; } >"$registry"
  chmod 600 "$registry"
}
assert_registry_size_limit() {
  local before
  rm -f -- "$desk_file"
  pad_registry 65537
  before="$(file_sum "$registry")"
  gateway_answers 200 200
  run_tui alpha --for-user desk
  refused_with "it is larger than 65536 bytes, the daemon's limit" \
    && [[ ! -e "$desk_file" && "$(file_sum "$registry")" == "$before" ]] || return 1
  run_tui alpha --list
  refused_with "refusing to list $registry: it is larger than 65536 bytes" || return 1
  pad_registry 65536
  run_tui alpha --list
  ((RC == 0)) && grep -Eq '^  tui:pad +- +-$' "$OUT" || return 1
  before="$(file_sum "$registry")"
  run_tui alpha --for-user desk
  refused_with "the result would be larger than 65536 bytes" \
    && [[ ! -e "$desk_file" && "$(file_sum "$registry")" == "$before" ]]
}
check "registry over the daemon's 64 KiB limit is refused before jq reads it; a grant never writes past the limit" \
  assert_registry_size_limit

refused_registry_file() {  # <fifo|hardlink|loose-mode|dangling-symlink>
  local ok=true
  rm -f -- "$desk_file" "$registry" "$fixture/registry-link"
  case "$1" in
    fifo) mkfifo -m 600 "$registry" ;;
    hardlink) seed_registry '{"version":1,"tokens":[]}' && ln -- "$registry" "$fixture/registry-link" ;;
    loose-mode) seed_registry '{"version":1,"tokens":[]}' && chmod 640 "$registry" ;;
    dangling-symlink) ln -s -- "$fixture/elsewhere/missing.json" "$registry" ;;
  esac
  gateway_answers 200 200
  run_tui_bounded 8 alpha --for-user desk
  refused_with "it is not a private regular file of the tenant" && [[ ! -e "$desk_file" ]] || ok=false
  run_tui_bounded 8 alpha --list
  refused_with "refusing to list $registry: it is not a private regular file" || ok=false
  rm -f -- "$registry" "$fixture/registry-link"
  [[ "$ok" == true ]]
}
check "a FIFO registry is refused at once instead of blocking the verb under the lifecycle lock" \
  refused_registry_file fifo
check "a hard-linked registry is refused before anything is written" refused_registry_file hardlink
check "a registry with group or other access is refused (the daemon would not load it)" \
  refused_registry_file loose-mode
check "a dangling-symlink registry is refused" refused_registry_file dangling-symlink

assert_fifo_master_and_revoke() {
  local ok=true
  rm -f -- "$desk_file" "$registry"
  mkfifo -m 600 "$registry"
  gateway_answers 200 200
  run_tui_bounded 8 alpha --for-user desk --use-master-token --no-verify
  ((RC == 0)) && [[ "$(desk_json .token_kind)" == master ]] \
    && grep -qF "an earlier client token 'tui:desk' may still be registered" "$ERR" || ok=false
  run_tui_bounded 8 alpha --for-user desk --revoke
  ((RC == 1)) && [[ ! -e "$desk_file" ]] && grep -qF 'it is not a private regular file' "$ERR" || ok=false
  rm -f -- "$registry" "$desk_file"
  [[ "$ok" == true ]]
}
check "with a FIFO registry a master grant only warns, and revoke removes the desktop file then exits 1" \
  assert_fifo_master_and_revoke

assert_revoke_without_desktop_read() {  # <fifo|oversized>
  local how="$1" ok=true
  clean_grant_state
  gateway_answers 200 200
  run_tui alpha --for-user desk --no-verify
  ((RC == 0)) && [[ -f "$desk_file" ]] || return 1
  rm -f -- "$desk_file"
  case "$how" in
    fifo) mkfifo -m 600 "$desk_file" ;;
    oversized) truncate -s 2G "$desk_file" ;;
  esac
  run_tui_bounded 8 alpha --for-user desk --revoke
  ((RC == 0)) && [[ ! -e "$desk_file" ]] \
    && grep -qF 'token_kind unknown; not read' "$OUT" \
    && grep -qF 'may have held the master token' "$ERR" \
    && ! drop_began desk "jq " || ok=false
  rm -f -- "$desk_file"
  [[ "$ok" == true ]]
}
check "UZ-6: revoke removes a desktop FIFO without opening it and reports unknown token kind" \
  assert_revoke_without_desktop_read fifo
check "UZ-6: revoke removes a multi-GB desktop file without reading it" \
  assert_revoke_without_desktop_read oversized

# ── Scenario L: listener owned by another uid ────────────────────────────────

clean_grant_state
write_proc_net 1234
gateway_answers 200 200
run_tui alpha --for-user desk
assert_20() {
  ((RC == 0)) && [[ "$(status_calls)" == 0 ]] && ! compgen -G "$calls/curl-config.*" >/dev/null \
    && ! grep -q 'Authorization' "$calls/curl.argv" \
    && grep -q 'gateway port 20042 is held by uid 1234, not alpha (1500); skipping the authenticated probe' "$ERR" \
    && [[ -f "$desk_file" ]] \
    && [[ "$(command jq -r '.tokens[] | select(.label == "tui:desk") | .sha256' "$registry")" == "$(digest_of "$(desk_json .token)")" ]]
}
check "20. probe skipped when the port owner uid differs: no Authorization header recorded, warning printed, exit 0, files kept" assert_20

write_proc_net 1500 '   1: 00000000000000000000000001000000:4E4A 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1777        0 31341 1 0000000000000000 100 0 0 10 0'
gateway_answers 200 200
run_tui alpha --for-user desk
check "a second uid on the port ([::1]:20042) also skips the authenticated probe" \
  eval '((RC == 0)) && [[ "$(status_calls)" == 0 ]] && grep -q "held by several uids" "$ERR"'
write_proc_net 1500

# ── Scenario L2: the owner check covers the address the probe connects to ────

set_listeners() {  # <4|6> <local address:port in /proc hex> <uid> [...]: the only rows
  local file remote
  printf '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n' \
    >"$TUI_PROC_NET_DIR/tcp"
  printf '  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n' \
    >"$TUI_PROC_NET_DIR/tcp6"
  while (($# >= 3)); do
    file="$TUI_PROC_NET_DIR/tcp" remote=00000000:0000
    if [[ "$1" == 6 ]]; then
      file="$TUI_PROC_NET_DIR/tcp6" remote="$(printf '%032d' 0):0000"
    fi
    printf '   0: %s %s 0A 00000000:00000000 00:00000000 00000000  %s        0 31400 1 0000000000000000 100 0 0 10 0\n' \
      "$2" "$remote" "$3" >>"$file"
    shift 3
  done
}
grant_probing() {  # <tui-access grant args...>: a grant against a healthy gateway
  clean_grant_state
  gateway_answers 200 200
  run_tui alpha --for-user desk "$@"
}
token_withheld() {
  ((RC == 0)) && [[ "$(status_calls)" == 0 ]] && ! compgen -G "$calls/curl-config.*" >/dev/null \
    && ! grep -q 'Authorization' "$calls/curl.argv"
}
token_verified() {
  ((RC == 0)) && [[ "$(status_calls)" == 1 ]] && grep -q 'verify:  ok (GET /api/gateway/status -> 200)' "$OUT"
}

assert_address_spelling() {
  local any6
  any6="$(printf '%032d' 0)"
  _tui_listener_targets 127.0.0.1 && [[ "$TUI_REACH_V4" == "0100007F 00000000" ]] \
    && [[ "$TUI_REACH_V6" == "0000000000000000FFFF00000100007F $any6" ]] || return 1
  _tui_listener_targets '[::1]' && [[ -z "$TUI_REACH_V4" ]] \
    && [[ "$TUI_REACH_V6" == "00000000000000000000000001000000 $any6" ]] || return 1
  _tui_listener_targets '[fd00::5]' && [[ "$TUI_REACH_V6" == "000000FD000000000000000005000000 $any6" ]] || return 1
  _tui_listener_targets '[::ffff:127.0.0.1]' && [[ "$TUI_REACH_V4" == "0100007F 00000000" ]] || return 1
  _tui_listener_targets gw.example && [[ "$TUI_REACH_ANY" == true ]] || return 1
  ! _tui_listener_targets '' && ! _tui_listener_targets '[1:::2]' \
    && ! _tui_listener_targets 127.0.0.010 && ! _tui_listener_targets 0x7f.1
}
check "SEC-01: /proc spellings of 127.0.0.1, ::1 and ::ffff:127.0.0.1 match design 3.8.1; odd IPv4 spellings never match" \
  assert_address_spelling

assert_owner_other_address() {
  set_listeners 4 0500007F:4E4A 1500 # alpha holds 127.0.0.5:20042 only
  grant_probing --url-host 127.0.0.1
  token_withheld \
    && grep -qF 'no listener on gateway port 20042 covers 127.0.0.1, the address the probe connects to; skipping the authenticated probe' "$ERR" \
    && grep -qF 'owner:   no listener on port 20042 covers 127.0.0.1 now' "$OUT"
}
check "SEC-01: alpha on 127.0.0.5 only, URL 127.0.0.1 -> no token sent (the port number alone proves nothing)" \
  assert_owner_other_address

assert_owner_other_family() {
  set_listeners 4 00000000:4E4A 1500 # IPv4 0.0.0.0 only
  grant_probing --url-host ::1
  token_withheld && grep -qF 'covers [::1], the address the probe connects to' "$ERR" || return 1
  set_listeners 6 00000000000000000000000001000000:4E4A 1500 # [::1] only
  grant_probing
  token_withheld && grep -qF 'covers 127.0.0.1, the address the probe connects to' "$ERR"
}
check "SEC-01: an IPv4 0.0.0.0 socket does not cover [::1], and a [::1] socket does not cover 127.0.0.1" \
  assert_owner_other_family

assert_owner_covering_rows() {
  local row
  for row in "4 0100007F:4E4A" "6 $(printf '%032d' 0):4E4A" "6 0000000000000000FFFF00000100007F:4E4A"; do
    set_listeners "${row%% *}" "${row#* }" 1500
    grant_probing
    token_verified || return 1
  done
  set_listeners 6 00000000000000000000000001000000:4E4A 1500
  grant_probing --url-host ::1
  token_verified
}
check "SEC-01: rows on 127.0.0.1, :: and ::ffff:127.0.0.1 cover 127.0.0.1; a ::1 row covers [::1]" \
  assert_owner_covering_rows

assert_owner_big_endian() {
  local ok=true
  FAKE_BYTE_ORDER=big
  set_listeners 4 7F000001:4E4A 1500
  grant_probing
  token_verified || ok=false
  set_listeners 4 0100007F:4E4A 1500 # 1.0.0.127 on a big-endian host
  grant_probing
  token_withheld || ok=false
  FAKE_BYTE_ORDER=""
  [[ "$ok" == true ]]
}
check "SEC-01: /proc addresses are read in host byte order (7F000001 is 127.0.0.1 on big-endian hosts)" \
  assert_owner_big_endian

assert_owner_odd_spelling() {
  set_listeners 4 00000000:4E4A 1500
  grant_probing --url-host 127.0.0.010
  token_withheld && grep -qF 'authenticated probe skipped (non-loopback URL' "$OUT" \
    && grep -qF 'is not a loopback URL' "$ERR"
}
check "SEC-01: a loopback spelling that curl decodes as octal (127.0.0.010) never receives the token" \
  assert_owner_odd_spelling
write_proc_net 1500

# ── Scenario M: hostile registry content in --list ───────────────────────────

seed_registry '{"version":1,"tokens":[{"label":"tui:\u001b[31mevil","sha256":"'"$(printf '%064d' 1)"'"},{"label":"tui:ok","sha256":"'"$(printf '%064d' 2)"'","created_at":"2026-11-01T00:00:00Z\n"},{"label":"tui:fine","sha256":"'"$(printf '%064d' 3)"'"}]}'
run_tui alpha --list
assert_21() {
  ((RC == 0)) && grep -qxF '  <invalid entry #1>' "$OUT" && grep -qxF '  <invalid entry #2>' "$OUT" \
    && grep -Eq '^  tui:fine +- +-$' "$OUT" \
    && ! grep -q $'\x1b' "$OUT" && ! grep -q $'\x1b' "$ERR" && ! grep -q evil "$OUT"
}
check "21. --list with an ESC byte in a label prints <invalid entry #1>" assert_21

# ── Scenario N: argument parsing ─────────────────────────────────────────────

assert_expiry_needs_a_value() {
  local before
  clean_grant_state
  before="$(file_sum "$registry")"
  gateway_answers 200 200
  run_tui alpha --for-user desk --expires-in-days ""
  refused_with "--expires-in-days requires a number of days" \
    && [[ ! -e "$desk_file" && "$(file_sum "$registry")" == "$before" ]] || return 1
  run_tui alpha --for-user desk --use-master-token --expires-in-days ""
  refused_with "--expires-in-days requires a number of days" && [[ ! -e "$desk_file" ]] || return 1
  run_tui alpha --for-user desk --revoke --expires-in-days ""
  refused_with "--expires-in-days requires a number of days"
}
check "--expires-in-days with an empty value is refused instead of granting a token that never expires" \
  assert_expiry_needs_a_value

# ── Dispatcher ───────────────────────────────────────────────────────────────

run_main() {  # <main args...>; errexit on, as when the script runs for real
  set +e
  (
    set -e
    TEST_IN_VERB=true
    main "$@"
  ) >"$OUT" 2>"$ERR"
  RC=$?
  set -e
}
run_main tui-access
check "dispatcher: tui-access without a tenant prints its usage" refused_with "usage: tui-access <name> --for-user <user>"
check "dispatcher: usage lists the tui-access verb" \
  eval 'usage | grep -q "tui-access <name> --for-user <user> --revoke"'
seed_registry '{"version":1,"tokens":[]}'
run_main tui-access alpha --list
check "dispatcher: tui-access alpha --list reaches the verb" \
  eval '((RC == 0)) && grep -q "client tokens registered for tenant '"'"'alpha'"'"'" "$OUT"'

# ── Every run: no root-side mutation of a desktop or tenant path ─────────────

assert_no_undropped() {
  [[ -s "$undropped" ]] || return 0
  sed 's/^/      undropped: /' -- "$undropped"
  return 1
}
check "drops: no run changed a desktop or tenant path outside a sudo -u drop to its owner" assert_no_undropped

if ((failures != 0)); then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all tui-access tests passed\n'
