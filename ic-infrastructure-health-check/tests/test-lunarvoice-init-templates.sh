#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SYSTEMD_DIRECT="$ROOT_DIR/projects/voice-sidecar/systemd/voice-sidecar.service"
OPENRC="$ROOT_DIR/projects/voice-sidecar/init.d/voice-sidecar"
COMPOSE="$ROOT_DIR/projects/voice-sidecar/compose.yaml"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/lunarvoice-init-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }
skip() { printf 'SKIP: %s\n' "$1"; }

assert_contains() {
    local file="$1" needle="$2" label="$3"
    grep -qF -- "$needle" "$file" \
        && ok "$label" \
        || bad "$label" "missing: $needle"
}

assert_absent() {
    local file="$1" needle="$2" label="$3"
    if grep -qF -- "$needle" "$file"; then
        bad "$label" "unexpected: $needle"
    else
        ok "$label"
    fi
}

assert_log_contains() {
    local needle="$1" label="$2"
    grep -qF -- "$needle" "$CALL_LOG" \
        && ok "$label" \
        || bad "$label" "missing call: $needle"
}

# Direct-binary system service: external secret env, bounded restarts, and a
# hardened runtime. The token must never be committed into the unit.
assert_contains "$SYSTEMD_DIRECT" 'EnvironmentFile=/etc/lunarwing/voice-sidecar.env' \
    "direct systemd unit requires external env file"
assert_contains "$SYSTEMD_DIRECT" 'NoNewPrivileges=true' \
    "direct systemd unit enables no-new-privileges"
assert_contains "$SYSTEMD_DIRECT" 'ProtectSystem=strict' \
    "direct systemd unit keeps the filesystem read-only"
assert_contains "$SYSTEMD_DIRECT" 'ReadOnlyPaths=/var/lib/lunarvoice/models' \
    "direct systemd unit mounts the model store read-only"
assert_contains "$SYSTEMD_DIRECT" 'Restart=on-failure' \
    "direct systemd unit restarts on failure"
assert_contains "$SYSTEMD_DIRECT" 'StartLimitIntervalSec=300' \
    "direct systemd unit bounds restart storms"
assert_contains "$SYSTEMD_DIRECT" 'TimeoutStopSec=30' \
    "direct systemd unit bounds stop time"
assert_contains "$SYSTEMD_DIRECT" 'UMask=0077' \
    "direct systemd unit creates owner-only files"
assert_absent "$SYSTEMD_DIRECT" 'VOICE_AUTH_TOKEN=' \
    "direct systemd unit embeds no auth token"

# Parse the unit. Substitute the not-yet-installed binary and service account
# only for verification so systemd-analyze validates directives instead of host
# state.
if command -v systemd-analyze >/dev/null 2>&1; then
    mkdir -p "$TMP/system"
    while IFS= read -r line; do
        line="${line//\/opt\/lunarvoice\/lunarwing-voice-sidecar//bin/true}"
        line="${line//User=lunarvoice/User=root}"
        line="${line//Group=lunarvoice/Group=root}"
        printf '%s\n' "$line"
    done <"$SYSTEMD_DIRECT" >"$TMP/system/voice-sidecar.service"
    if systemd-analyze verify "$TMP/system/voice-sidecar.service" >/dev/null 2>&1; then
        ok "direct systemd unit passes systemd-analyze"
    else
        bad "direct systemd unit passes systemd-analyze" "unit parser rejected template"
    fi
else
    # systemd-analyze only exists on systemd hosts (not the CI lint image or
    # OpenRC dev boxes); the static-contract assertions above still apply, so
    # treat the parser check as skipped rather than failed here.
    skip "systemd template parsed (systemd-analyze not installed)"
fi

# Compose contract: loopback-only publishing, container health gate on the
# dedicated listener, and no-new-privileges.
assert_contains "$COMPOSE" '"127.0.0.1:8090:8090"' \
    "compose publishes the API port on loopback"
assert_contains "$COMPOSE" '"127.0.0.1:8091:8091"' \
    "compose publishes the health port on loopback"
assert_contains "$COMPOSE" 'no-new-privileges:true' \
    "compose enables no-new-privileges"
assert_contains "$COMPOSE" 'http://127.0.0.1:8091/health' \
    "compose health-checks the dedicated listener"
assert_absent "$COMPOSE" 'VOICE_AUTH_TOKEN=' \
    "compose embeds no auth token"

# OpenRC static contract.
assert_contains "$OPENRC" 'supervisor="supervise-daemon"' \
    "OpenRC uses supervise-daemon"
assert_contains "$OPENRC" 'command_user="${voice_user}:${voice_group}"' \
    "OpenRC runs the daemon as the service account"
assert_contains "$OPENRC" 'environment file must have mode 0600' \
    "OpenRC validates secret env mode"
assert_contains "$OPENRC" 'http://127.0.0.1:${VOICE_HEALTH_PORT:-8091}/health' \
    "OpenRC startup gates on the dedicated health listener"
assert_absent "$OPENRC" 'VOICE_AUTH_TOKEN=' \
    "OpenRC unit embeds no auth token"

# Execute the OpenRC lifecycle with mocked init/runtime primitives. This
# verifies env validation ordering and the health gate without an OpenRC host.
mkdir -p "$TMP/bin" "$TMP/run"
ENV_FILE="$TMP/voice-sidecar.env"
printf 'VOICE_AUTH_TOKEN=test-only-token\nVOICE_HEALTH_PORT=9999\n' >"$ENV_FILE"
chmod 0600 "$ENV_FILE"
CALL_LOG="$TMP/calls.log"
: >"$CALL_LOG"

# shellcheck disable=SC2034  # Consumed by the sourced OpenRC script.
voice_env_file="$ENV_FILE"
# shellcheck disable=SC2034  # Consumed by the sourced OpenRC script.
voice_wait=2
# shellcheck disable=SC2034  # Consumed by the sourced OpenRC script.
output_log="$TMP/run/voice.log"
# shellcheck disable=SC2034  # Consumed by the sourced OpenRC script.
error_log="$TMP/run/voice.err"
# shellcheck source=../../projects/voice-sidecar/init.d/voice-sidecar
source "$OPENRC"

checkpath() { printf 'checkpath:%s\n' "$*" >>"$CALL_LOG"; }
eerror() { printf 'eerror:%s\n' "$*" >>"$CALL_LOG"; }
ebegin() { printf 'ebegin:%s\n' "$*" >>"$CALL_LOG"; }
eend() { printf 'eend:%s\n' "$*" >>"$CALL_LOG"; return "${1:-0}"; }
curl() { printf 'curl:%s\n' "$*" >>"$CALL_LOG"; [[ "${MOCK_CURL_FAIL:-false}" != true ]]; }

if start_pre; then
    ok "OpenRC start_pre accepts valid mode-0600 env"
else
    bad "OpenRC start_pre accepts valid mode-0600 env" "start_pre returned non-zero"
fi
assert_log_contains "checkpath:-d -m 0750 -o lunarvoice:lunarvoice /run/lunarvoice" \
    "OpenRC start_pre prepares the runtime directory"

if start_post; then
    ok "OpenRC start_post accepts healthy sidecar"
else
    bad "OpenRC start_post accepts healthy sidecar" "health gate returned non-zero"
fi
assert_log_contains 'curl:--fail --silent --output /dev/null --max-time 3 http://127.0.0.1:9999/health' \
    "OpenRC post-start probes the env-configured health port"

: >"$CALL_LOG"
if MOCK_CURL_FAIL=true start_post >/dev/null 2>&1; then
    bad "OpenRC start_post fails when sidecar never healthy" "unhealthy sidecar accepted"
else
    ok "OpenRC start_post fails when sidecar never healthy"
fi
assert_log_contains "eend:1 not healthy after 2s" \
    "OpenRC post-start explains the health failure"

chmod 0644 "$ENV_FILE"
: >"$CALL_LOG"
if start_pre >/dev/null 2>&1; then
    bad "OpenRC rejects loose env permissions" "mode 0644 was accepted"
else
    ok "OpenRC rejects loose env permissions"
fi
if grep -q '^checkpath:' "$CALL_LOG"; then
    bad "OpenRC rejects env before filesystem mutation" "checkpath ran after invalid env"
else
    ok "OpenRC rejects env before filesystem mutation"
fi
chmod 0600 "$ENV_FILE"

mv "$ENV_FILE" "$ENV_FILE.real"
ln -s "$ENV_FILE.real" "$ENV_FILE"
if start_pre >/dev/null 2>&1; then
    bad "OpenRC rejects symlinked env file" "symlink was accepted"
else
    ok "OpenRC rejects symlinked env file"
fi
rm -f "$ENV_FILE"
mv "$ENV_FILE.real" "$ENV_FILE"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
