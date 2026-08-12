#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SYSTEMD_USER="$ROOT_DIR/ic-infrastructure-health-check/init-templates/ocr-sidecar.service"
SYSTEMD_DIRECT="$ROOT_DIR/projects/ocr-sidecar/systemd/ocr-sidecar.service"
OPENRC="$ROOT_DIR/ic-infrastructure-health-check/init-templates/ocr-sidecar"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/lunarvision-init-test.XXXXXX")"
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

# systemd user unit: foreground Compose lifecycle, required secret env, and a
# health-gated start on the dedicated operational listener.
assert_contains "$SYSTEMD_USER" 'EnvironmentFile=%h/.config/lunarwing/ocr-sidecar.env' \
    "systemd user unit requires per-user env file"
assert_contains "$SYSTEMD_USER" 'ExecStartPre=-/usr/bin/podman compose down --remove-orphans' \
    "systemd user cleanup uses systemd ignore-failure prefix"
assert_contains "$SYSTEMD_USER" 'http://127.0.0.1:8089/health' \
    "systemd user startup gates on dedicated health listener"
assert_contains "$SYSTEMD_USER" 'Restart=on-failure' \
    "systemd user unit restarts failed Compose runs"
assert_absent "$SYSTEMD_USER" '|| true' \
    "systemd directives contain no shell-only boolean syntax"
assert_absent "$SYSTEMD_USER" 'LUNARWING_AUTH_TOKEN=' \
    "systemd user unit embeds no auth token"

# Direct-binary system service: no committed token and a constrained DynamicUser.
assert_contains "$SYSTEMD_DIRECT" 'DynamicUser=yes' \
    "direct systemd unit uses DynamicUser"
assert_contains "$SYSTEMD_DIRECT" 'EnvironmentFile=/etc/lunarwing/ocr-sidecar.env' \
    "direct systemd unit requires external env file"
assert_contains "$SYSTEMD_DIRECT" 'NoNewPrivileges=true' \
    "direct systemd unit enables no-new-privileges"
assert_absent "$SYSTEMD_DIRECT" 'LUNARWING_AUTH_TOKEN=' \
    "direct systemd unit embeds no auth token"

# Parse both unit files. Substitute the not-yet-installed direct binary only for
# verification so systemd-analyze validates directives instead of host state.
if command -v systemd-analyze >/dev/null 2>&1; then
    mkdir -p "$TMP/user" "$TMP/system"
    cp "$SYSTEMD_USER" "$TMP/user/ocr-sidecar.service"
    while IFS= read -r line; do
        printf '%s\n' "${line//\/usr\/local\/bin\/lunarwing-ocr-sidecar/\/bin\/true}"
    done <"$SYSTEMD_DIRECT" >"$TMP/system/ocr-sidecar.service"
    if systemd-analyze --user verify "$TMP/user/ocr-sidecar.service" >/dev/null 2>&1; then
        ok "systemd user unit passes systemd-analyze"
    else
        bad "systemd user unit passes systemd-analyze" "unit parser rejected template"
    fi
    if systemd-analyze verify "$TMP/system/ocr-sidecar.service" >/dev/null 2>&1; then
        ok "direct systemd unit passes systemd-analyze"
    else
        bad "direct systemd unit passes systemd-analyze" "unit parser rejected template"
    fi
else
    # systemd-analyze only exists on systemd hosts (not the CI lint image or
    # OpenRC dev boxes); the static-contract assertions above still apply, so
    # treat the parser check as skipped rather than failed here.
    skip "systemd templates parsed (systemd-analyze not installed)"
fi

# OpenRC static contract.
assert_contains "$OPENRC" 'supervisor="supervise-daemon"' \
    "OpenRC uses supervise-daemon"
assert_contains "$OPENRC" 'command_user="${ocr_user}:${ocr_user}"' \
    "OpenRC runs Compose as service account"
assert_contains "$OPENRC" 'compose --env-file ${ocr_env_file} up --build --remove-orphans' \
    "OpenRC passes secret env as Compose data"
assert_contains "$OPENRC" 'http://127.0.0.1:8089/health' \
    "OpenRC startup gates on dedicated health listener"
assert_contains "$OPENRC" 'environment file must have mode 0600' \
    "OpenRC validates secret env mode"
assert_absent "$OPENRC" '. "${ocr_env_file}"' \
    "OpenRC never shell-sources secret env"

# Execute OpenRC lifecycle functions with mocked init/runtime primitives. This
# verifies argument ordering and fail-loud env validation without an OpenRC host.
mkdir -p "$TMP/home/.config/lunarwing" "$TMP/compose" "$TMP/log"
ENV_FILE="$TMP/home/.config/lunarwing/ocr-sidecar.env"
printf 'LUNARWING_AUTH_TOKEN=test-only-token\n' >"$ENV_FILE"
chmod 0600 "$ENV_FILE"
CALL_LOG="$TMP/calls.log"
: >"$CALL_LOG"

# ShellCheck cannot see these globals consumed by the sourced OpenRC functions.
# shellcheck disable=SC2034
ocr_user="$(id -un)"
# shellcheck disable=SC2034
ocr_home="$TMP/home"
# shellcheck disable=SC2034
ocr_env_file="$ENV_FILE"
# shellcheck disable=SC2034
directory="$TMP/compose"
# shellcheck disable=SC2034
output_log="$TMP/log/ocr-sidecar.log"
# shellcheck disable=SC2034
error_log="$TMP/log/ocr-sidecar.err"
# shellcheck disable=SC2034
ocr_wait=2
# shellcheck source=../init-templates/ocr-sidecar
source "$OPENRC"

checkpath() { printf 'checkpath:%s\n' "$*" >>"$CALL_LOG"; }
eerror() { printf 'eerror:%s\n' "$*" >>"$CALL_LOG"; }
ebegin() { printf 'ebegin:%s\n' "$*" >>"$CALL_LOG"; }
eend() { printf 'eend:%s\n' "$*" >>"$CALL_LOG"; return "${1:-0}"; }
_ocr() { printf 'ocr:%s\n' "$*" >>"$CALL_LOG"; return 0; }

if start_pre; then
    ok "OpenRC start_pre accepts valid mode-0600 env"
else
    bad "OpenRC start_pre accepts valid mode-0600 env" "start_pre returned non-zero"
fi
assert_log_contains "ocr:compose --env-file $ENV_FILE down --remove-orphans" \
    "OpenRC pre-start removes stale Compose resources"

if start_post; then
    ok "OpenRC start_post accepts healthy sidecar"
else
    bad "OpenRC start_post accepts healthy sidecar" "health gate returned non-zero"
fi
assert_log_contains 'ocr:exec lunarwing-ocr-sidecar curl --fail --silent --output /dev/null --max-time 3 http://127.0.0.1:8089/health' \
    "OpenRC post-start probes dedicated listener"

if stop_post; then
    ok "OpenRC stop_post removes Compose resources"
else
    bad "OpenRC stop_post removes Compose resources" "stop_post returned non-zero"
fi
assert_log_contains "ocr:compose --env-file $ENV_FILE down" \
    "OpenRC shutdown uses the same env file"

chmod 0644 "$ENV_FILE"
: >"$CALL_LOG"
if start_pre >/dev/null 2>&1; then
    bad "OpenRC rejects loose env permissions" "mode 0644 was accepted"
else
    ok "OpenRC rejects loose env permissions"
fi
if grep -q '^ocr:' "$CALL_LOG"; then
    bad "OpenRC rejects env before runtime mutation" "runtime was called after invalid env"
else
    ok "OpenRC rejects env before runtime mutation"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
