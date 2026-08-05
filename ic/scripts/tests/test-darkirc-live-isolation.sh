#!/usr/bin/env bash
# Live H3 evidence for the DarkIRC Stage A key exchange: real two-user
# isolation plus live /proc, stdout/stderr, and service-log secret-leak scans
# on an OpenRC host. See docs/ops/DARKIRC-KEYEXCHANGE-STATUS.md.
#
# This suite requires passwordless sudo (it creates two scratch OS users and
# installs root-owned test assets), jq, strace exec-delay support, a real
# DarkIRC daemon, and an explicitly trusted helper with its source revision and
# independently recorded digest. It skips cleanly (exit 0) when any are
# unavailable, so a skip is never acceptance evidence.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
IC_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
HELPER_SRC="${DARKIRC_LIVE_HELPER:-}"
HELPER_SOURCE_REVISION="${DARKIRC_LIVE_HELPER_SOURCE_REVISION:-}"
HELPER_EXPECTED_SHA256="${DARKIRC_LIVE_HELPER_SHA256:-}"
DAEMON_SRC="${DARKIRC_LIVE_DAEMON:-/usr/local/bin/darkirc}"
DAEMON_EXPECTED_SHA256="${DARKIRC_LIVE_DAEMON_SHA256:-}"

log() { printf 'LIVE: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
skip() { printf 'SKIP: %s\n' "$*"; exit 0; }

trusted_executable_source() {
  local path="$1" current mode owner links
  [[ "$path" == /* && "$(realpath -e -- "$path" 2>/dev/null)" == "$path" ]] || return 1
  [[ -x "$path" && -f "$path" && ! -L "$path" ]] || return 1
  owner="$(stat -c '%u' "$path" 2>/dev/null)" || return 1
  links="$(stat -c '%h' "$path" 2>/dev/null)" || return 1
  mode="$(stat -c '%a' "$path" 2>/dev/null)" || return 1
  [[ "$owner" == 0 && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || return 1
  (( (8#$mode & 07022) == 0 )) || return 1
  current="$(dirname "$path")"
  while [[ "$current" != / ]]; do
    owner="$(stat -c '%u' "$current" 2>/dev/null)" || return 1
    mode="$(stat -c '%a' "$current" 2>/dev/null)" || return 1
    [[ "$owner" == 0 && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 0022) == 0 )) || return 1
    current="$(dirname "$current")"
  done
}

command -v git >/dev/null 2>&1 || skip "git is required"
command -v realpath >/dev/null 2>&1 || skip "realpath is required"
command -v sha256sum >/dev/null 2>&1 || skip "sha256sum is required"
[[ -n "$HELPER_SRC" ]] && trusted_executable_source "$HELPER_SRC" \
  || skip "DARKIRC_LIVE_HELPER must be root-owned, canonical, single-link, and non-writable"
current_source_revision="$(git -C "$IC_ROOT/.." rev-parse HEAD 2>/dev/null || true)"
[[ "$HELPER_SOURCE_REVISION" == "$current_source_revision" \
  && "$HELPER_SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]] \
  || skip "set DARKIRC_LIVE_HELPER_SOURCE_REVISION to current trusted source revision"
[[ "$HELPER_EXPECTED_SHA256" =~ ^sha256:[0-9a-f]{64}$ \
  && "$HELPER_EXPECTED_SHA256" == "sha256:$(sha256sum "$HELPER_SRC" | cut -d' ' -f1)" ]] \
  || skip "set DARKIRC_LIVE_HELPER_SHA256 to trusted helper digest"
trusted_executable_source "$DAEMON_SRC" \
  || skip "DARKIRC_LIVE_DAEMON must be root-owned, canonical, single-link, and non-writable"
[[ "$DAEMON_EXPECTED_SHA256" =~ ^sha256:[0-9a-f]{64}$ \
  && "$DAEMON_EXPECTED_SHA256" == "sha256:$(sha256sum "$DAEMON_SRC" | cut -d' ' -f1)" ]] \
  || skip "set DARKIRC_LIVE_DAEMON_SHA256 to trusted daemon digest"
command -v jq >/dev/null 2>&1 || skip "jq is required"
command -v sudo >/dev/null 2>&1 || skip "sudo is required"
command -v useradd >/dev/null 2>&1 || skip "useradd is required"
command -v strace >/dev/null 2>&1 || skip "strace with exec-delay injection is required"
strace -f -qq -e trace=execve -e inject=execve:delay_exit=1ms \
  -o /dev/null /bin/true 2>/dev/null \
  || skip "strace exec-delay injection is unavailable"
sudo -n true 2>/dev/null || skip "passwordless sudo is required"

if [[ "$(id -u)" != 0 ]]; then
  exec sudo -n env DARKIRC_LIVE_KEEP="${DARKIRC_LIVE_KEEP:-0}" \
    DARKIRC_LIVE_DAEMON="$DAEMON_SRC" DARKIRC_LIVE_HELPER="$HELPER_SRC" \
    DARKIRC_LIVE_HELPER_SOURCE_REVISION="$HELPER_SOURCE_REVISION" \
    DARKIRC_LIVE_HELPER_SHA256="$HELPER_EXPECTED_SHA256" \
    DARKIRC_LIVE_DAEMON_SHA256="$DAEMON_EXPECTED_SHA256" bash "$0" "$@"
fi

USER_A="dklive-a"
USER_B="dklive-b"
A_SCOPE="11111111111111111111111111111111"
B_SCOPE="22222222222222222222222222222222"
WORK="/var/lib/darkirc-live-test"
RELEASES_DIR="$WORK/releases"
GENERATOR=""
HELPER=""
COMPAT_FILE=""
SERVICE="darkirc-live-scan"
SERVICE_SCRIPT="/etc/init.d/$SERVICE"
SERVICE_WRAPPER="/usr/local/libexec/darkirc-live-invoke.sh"
SERVICE_LOG="/var/log/darkirc-live-scan.log"
REVISION="a05956d412a091e8b54c1cd4f4264c33b941203d"
POLLER_PID=""
TRACE_SEQ=0

for path in "$WORK" "$SERVICE_SCRIPT" "$SERVICE_WRAPPER"; do
  if [[ -e "$path" || -L "$path" ]]; then
    fail "refusing to overwrite existing asset: $path"
  fi
done
if id "$USER_A" >/dev/null 2>&1 || id "$USER_B" >/dev/null 2>&1; then
  fail "scratch users $USER_A/$USER_B already exist"
fi

cleanup() {
  set +e
  [[ -z "$POLLER_PID" ]] || kill "$POLLER_PID" 2>/dev/null
  rc-service "$SERVICE" stop >/dev/null 2>&1
  rc-service "$SERVICE" zap >/dev/null 2>&1
  rm -f "$SERVICE_SCRIPT" "$SERVICE_WRAPPER" "$SERVICE_LOG"
  userdel -r "$USER_A" >/dev/null 2>&1
  userdel -r "$USER_B" >/dev/null 2>&1
  if [[ "${DARKIRC_LIVE_KEEP:-0}" == 1 ]]; then
    rm -rf /tmp/darkirc-live-evidence
    cp -a "$WORK" /tmp/darkirc-live-evidence
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# --- Publish a root-owned immutable release v2 ----------------------------

install -d -o root -g root -m 0711 "$WORK"
install -d -o root -g root -m 0700 "$WORK/proc" "$WORK/capture"
install -d -o root -g root -m 0755 "$RELEASES_DIR"
daemon_digest="sha256:$(sha256sum "$DAEMON_SRC" | cut -d' ' -f1)"
helper_digest="sha256:$(sha256sum "$HELPER_SRC" | cut -d' ' -f1)"
helper_source_revision="$HELPER_SOURCE_REVISION"
release_id="$REVISION-${helper_source_revision:0:16}-${daemon_digest:7:16}-${helper_digest:7:16}"
release_dir="$RELEASES_DIR/$release_id"
install -d -o root -g root -m 0755 "$release_dir"
install -o root -g root -m 0555 "$DAEMON_SRC" "$release_dir/darkirc"
install -o root -g root -m 0555 "$HELPER_SRC" "$release_dir/lunarwing-darkirc-key-helper"
[[ "sha256:$(sha256sum "$release_dir/darkirc" | cut -d' ' -f1)" == "$DAEMON_EXPECTED_SHA256" ]] \
  || fail "installed daemon changed after trusted digest validation"
[[ "sha256:$(sha256sum "$release_dir/lunarwing-darkirc-key-helper" | cut -d' ' -f1)" \
  == "$HELPER_EXPECTED_SHA256" ]] \
  || fail "installed helper changed after trusted digest validation"
GENERATOR="$release_dir/darkirc"
HELPER="$release_dir/lunarwing-darkirc-key-helper"
COMPAT_FILE="$release_dir/attestation.json"
jq -n --arg release "$release_id" --arg daemon "$daemon_digest" \
  --arg helper "$helper_digest" --arg helper_revision "$helper_source_revision" \
  --arg revision "$REVISION" \
  '{schema:"lunarwing.darkirc-compat/v2",release_id:$release,profile_id:"darkfi-a05956d41-chacha-v1",daemon_sha256:$daemon,helper_sha256:$helper,helper_source_revision:$helper_revision,key_format:"darkfi-chacha-base58-32",source_revision:$revision}' \
  >"$COMPAT_FILE"
chown root:root "$COMPAT_FILE"
chmod 0444 "$COMPAT_FILE"
chmod 0555 "$release_dir"

COMPAT_ARGS=(--generator-profile darkfi-a05956d41-chacha-v1
  --binary-sha256 "$daemon_digest" --binary-path "$GENERATOR"
  --helper-sha256 "$helper_digest" --release-id "$release_id"
  --key-format darkfi-chacha-base58-32 --source-revision "$REVISION")
useradd -m -s /bin/bash "$USER_A"
useradd -m -s /bin/bash "$USER_B"
for user in "$USER_A" "$USER_B"; do
  chmod 0700 "/home/$user"
  sudo -n -u "$user" mkdir -p "/home/$user/lunarwing/state/darkirc"
  sudo -n -u "$user" chmod 0700 "/home/$user/lunarwing" \
    "/home/$user/lunarwing/state" "/home/$user/lunarwing/state/darkirc"
done
sudo -n -u "$USER_A" env -i HOME="/home/$USER_A" PATH="/usr/bin:/bin" \
  "$HELPER" self-test-generator "${COMPAT_ARGS[@]}" >/dev/null \
  || fail "real DarkIRC daemon failed unprivileged release-v2 generator self-test"

# --- Helpers ---------------------------------------------------------------

# Invoke the key helper exactly the way mt-admin does: dropped privileges,
# scrubbed environment, tenant home as cwd, attestation and identity pinning.
run_helper() {
  local user="$1"
  shift
  local uid gid trace
  uid="$(id -u "$user")"
  gid="$(id -g "$user")"
  TRACE_SEQ=$((TRACE_SEQ + 1))
  trace="$WORK/proc/run-$TRACE_SEQ.strace"
  ( cd "/home/$user" && strace -f -qq -v -s 1048576 -e trace=execve \
      -e inject=execve:delay_exit=50ms -o "$trace" \
      sudo -n -u "$user" env -i HOME="/home/$user" PATH="/usr/bin:/bin" \
      "$HELPER" "$@" "${COMPAT_ARGS[@]}" --expected-uid "$uid" --expected-gid "$gid" )
}

# Snapshot argv and environment of every helper/generator process while the
# exchange runs, so the captured surfaces can be scanned for secret material.
proc_poller() {
  while :; do
    local pid
    # shellcheck disable=SC2046
    for pid in $(pgrep -f 'lunarwing-darkirc-key-helper|--gen-chacha-keypair' 2>/dev/null || true); do
      [[ -r "/proc/$pid/cmdline" ]] || continue
      cat "/proc/$pid/cmdline" >"$WORK/proc/$pid.cmdline" 2>/dev/null || true
      cat "/proc/$pid/environ" >"$WORK/proc/$pid.environ" 2>/dev/null || true
      readlink "/proc/$pid/exe" >"$WORK/proc/$pid.exe" 2>/dev/null || true
    done
    sleep 0.05
  done
}

SECRETS=()

assert_no_secret() {
  local file="$1" label="$2" secret
  [[ -f "$file" ]] || return 0
  for secret in ${SECRETS[@]+"${SECRETS[@]}"}; do
    if grep -Faq -- "$secret" "$file"; then
      fail "private key material present in $label ($file)"
    fi
  done
}

# --- Live Stage A exchange across two real OS users ------------------------

proc_poller &
POLLER_PID=$!

run_helper "$USER_A" prepare --tenant "$USER_A" --scope-id "$A_SCOPE" --contact peer-b \
  --out offer.json --json >"$WORK/capture/a-prepare.out" 2>"$WORK/capture/a-prepare.err"
offer_id="$(jq -r '.offer_id' "$WORK/capture/a-prepare.out")"
fp_a="$(jq -r '.public_fingerprint' "$WORK/capture/a-prepare.out")"
[[ "$offer_id" =~ ^[0-9a-f]{32}$ ]] || fail "prepare returned no offer id"
[[ "$fp_a" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "prepare returned no fingerprint"
log "PASS: prepare as $USER_A (offer $offer_id)"

# Public artifacts cross between users through a root-staged copy, standing
# in for the out-of-band public channel.
install -o "$USER_B" -g "$USER_B" -m 0600 "/home/$USER_A/offer.json" "/home/$USER_B/inbox-offer.json"

run_helper "$USER_B" respond --tenant "$USER_B" --scope-id "$B_SCOPE" --contact peer-a \
  --expect-peer-fingerprint "$fp_a" --in inbox-offer.json --out response.json --json \
  >"$WORK/capture/b-respond.out" 2>"$WORK/capture/b-respond.err"
fp_b="$(jq -r '.public_fingerprint' "$WORK/capture/b-respond.out")"
[[ "$fp_b" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "respond returned no fingerprint"
log "PASS: respond as $USER_B"

install -o "$USER_A" -g "$USER_A" -m 0600 "/home/$USER_B/response.json" "/home/$USER_A/inbox-response.json"

run_helper "$USER_A" complete --tenant "$USER_A" --scope-id "$A_SCOPE" --contact peer-b \
  --exchange-id "$offer_id" --expect-peer-fingerprint "$fp_b" --in inbox-response.json \
  --defer-apply --json >"$WORK/capture/a-complete.out" 2>"$WORK/capture/a-complete.err"
log "PASS: deferred complete as $USER_A"

# Read-only operator surfaces on both tenants. These exist to be scanned for
# secret material, so a nonzero exit (for example a tenant without a ledger
# yet) is captured, not fatal.
for user in "$USER_A" "$USER_B"; do
  short="${user#dklive-}"
  scope="$A_SCOPE"
  [[ "$user" == "$USER_A" ]] || scope="$B_SCOPE"
  for op in list doctor exchanges ledger; do
    rc=0
    run_helper "$user" "$op" --tenant "$user" --scope-id "$scope" \
      >"$WORK/capture/$short-$op.out" 2>&1 || rc=$?
    log "surface $user $op exit=$rc"
  done
done
rc=0
run_helper "$USER_A" status --tenant "$USER_A" --contact peer-b \
  >"$WORK/capture/a-status.out" 2>&1 || rc=$?
log "surface $USER_A status exit=$rc"

# --- OpenRC-supervised execution with init-captured logs --------------------

openrc_status="unavailable (no OpenRC)"
if command -v rc-service >/dev/null 2>&1 && [[ -x /sbin/openrc-run ]]; then
  cat >"$SERVICE_WRAPPER" <<EOF
#!/bin/sh
cd /home/$USER_A || exit 1
uid="\$(id -u $USER_A)"
gid="\$(id -g $USER_A)"
exec sudo -n -u $USER_A env -i HOME=/home/$USER_A PATH=/usr/bin:/bin \
  $HELPER prepare --tenant $USER_A --scope-id $A_SCOPE --contact peer-openrc \
  --out openrc-offer.json --json \
  --generator-profile darkfi-a05956d41-chacha-v1 --binary-sha256 "$daemon_digest" \
  --binary-path $GENERATOR --helper-sha256 "$helper_digest" --release-id "$release_id" \
  --key-format darkfi-chacha-base58-32 \
  --source-revision $REVISION --expected-uid "\$uid" --expected-gid "\$gid"
EOF
  chown root:root "$SERVICE_WRAPPER"
  chmod 0755 "$SERVICE_WRAPPER"
  cat >"$SERVICE_SCRIPT" <<EOF
#!/sbin/openrc-run
name="$SERVICE"
description="DarkIRC Stage A live leak-scan invocation (one-shot)"
command="$SERVICE_WRAPPER"
command_background="true"
pidfile="/run/$SERVICE.pid"
output_log="$SERVICE_LOG"
error_log="$SERVICE_LOG"
EOF
  chown root:root "$SERVICE_SCRIPT"
  chmod 0755 "$SERVICE_SCRIPT"
  : >"$SERVICE_LOG"
  if rc-service "$SERVICE" start >/dev/null 2>&1; then
    for _ in $(seq 1 150); do
      grep -q '"offer_id"' "$SERVICE_LOG" 2>/dev/null && break
      sleep 0.1
    done
    rc-service "$SERVICE" stop >/dev/null 2>&1 || rc-service "$SERVICE" zap >/dev/null 2>&1 || true
    if grep -q '"offer_id"' "$SERVICE_LOG"; then
      openrc_status="executed (supervised run captured by init logging)"
    else
      openrc_status="unavailable (service produced no output)"
    fi
  else
    openrc_status="unavailable (rc-service start failed)"
  fi
fi
log "OpenRC execution: $openrc_status"

# --- Secret extraction and leak scans ---------------------------------------

pending_files=()
while IFS= read -r file; do
  pending_files+=("$file")
done < <(find "/home/$USER_A/lunarwing/state/darkirc" "/home/$USER_B/lunarwing/state/darkirc" \
  -path '*pending*' -name '*.json' -type f 2>/dev/null)
[[ "${#pending_files[@]}" -ge 2 ]] || fail "expected pending exchange records for both users"
while IFS= read -r secret; do
  [[ -z "$secret" ]] || SECRETS+=("$secret")
done < <(jq -r '.local_private // empty' "${pending_files[@]}" | sort -u)
[[ "${#SECRETS[@]}" -ge 2 ]] || fail "could not extract pending secrets for both users"
log "extracted ${#SECRETS[@]} distinct pending secrets from mode-0600 records (never printed)"

# Live /proc snapshots must exist and cover the helper and the generator.
snapshot_count="$(find "$WORK/proc" -name '*.cmdline' -type f | wc -l)"
[[ "$snapshot_count" -gt 0 ]] || fail "no live /proc snapshots captured"
grep -Fqal -- '--gen-chacha-keypair' "$WORK"/proc/*.cmdline || fail "generator process never observed"
grep -Fqal -- "lunarwing-darkirc-key-helper" "$WORK"/proc/*.cmdline || fail "helper process never observed"

for file in "$WORK"/capture/* "$WORK"/proc/*; do
  assert_no_secret "$file" "captured stdout/stderr or live /proc surface"
done
log "PASS: $snapshot_count live /proc argv/environment snapshots and all captured stdout/stderr are secret-free"

assert_no_secret "/home/$USER_A/offer.json" "public offer artifact"
assert_no_secret "/home/$USER_B/response.json" "public response artifact"
assert_no_secret "/home/$USER_A/lunarwing/state/darkirc/key-exchange/ledger.json" "ledger of $USER_A"
assert_no_secret "/home/$USER_B/lunarwing/state/darkirc/key-exchange/ledger.json" "ledger of $USER_B"
assert_no_secret "$SERVICE_LOG" "OpenRC supervised service log"
if [[ -f /var/log/messages ]]; then
  assert_no_secret /var/log/messages "system log (/var/log/messages, includes sudo argv records)"
fi
log "PASS: public artifacts, ledgers, service log, and system log are secret-free"

# --- Real cross-user isolation ----------------------------------------------

if sudo -n -u "$USER_B" cat "/home/$USER_A/lunarwing/state/darkirc/key-exchange/pending/"*.json \
  >/dev/null 2>&1; then
  fail "user $USER_B read the pending secret records of $USER_A"
fi
log "PASS: cross-user read of pending secret records is denied"

if run_helper "$USER_B" list --tenant "$USER_A" >"$WORK/capture/cross-user.out" 2>&1; then
  fail "user $USER_B operated the key helper on the state of $USER_A"
fi
assert_no_secret "$WORK/capture/cross-user.out" "cross-user rejection error surface"
log "PASS: cross-user helper operation on foreign tenant state is refused without leaking"

if find "/home/$USER_A/lunarwing" "/home/$USER_B/lunarwing" -perm /077 -print -quit | grep -q .; then
  fail "group/other accessible path exists under tenant DarkIRC state"
fi
log "PASS: tenant DarkIRC state trees carry no group/other permission bits"

log "evidence: ${#SECRETS[@]} secrets, $snapshot_count /proc snapshots, OpenRC: $openrc_status"
printf 'ALL LIVE ISOLATION TESTS PASSED\n'
