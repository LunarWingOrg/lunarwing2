#!/usr/bin/env bash
# Static + parser validation of the healthpoint metrics listener's units.
#
# The listener is loopback-only by construction in the binary; both units must
# run it without privileges, embed no tokens, and suggest no wildcard binds.
# systemd-analyze verify runs only when the tool exists (this suite is also
# green on OpenRC-only hosts).
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
UNIT="$ROOT_DIR/ic/systemd/lunarwing-health-metrics.service"
OPENRC_UNIT="$ROOT_DIR/ic/systemd/lunarwing-health-metrics.openrc"
CONFD="$ROOT_DIR/ic/systemd/lunarwing-health-metrics.confd"
INSTALLER="$ROOT_DIR/ic/scripts/install-health-metrics-listener.sh"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/health-metrics-unit-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }

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

[[ -f "$UNIT" ]] || { echo "FATAL: unit not found at $UNIT"; exit 1; }

# Loopback-only listener contract + delivery fallback note.
assert_contains "$UNIT" 'After=network.target' \
    "unit starts after network"
assert_contains "$UNIT" 'ExecStart=/usr/local/lib/lunarwing-health/lunarwing-health-metrics' \
    "unit execs the installed listener binary"
assert_contains "$UNIT" 'EnvironmentFile=-/etc/lunarwing/health.env' \
    "env file is optional (textfile exporter needs no listener)"
assert_contains "$UNIT" 'Restart=on-failure' \
    "listener restarts on failure"

# Hardening: the service only ever READS the exported .prom file.
assert_contains "$UNIT" 'DynamicUser=yes' \
    "listener runs as a dynamic user"
assert_contains "$UNIT" 'NoNewPrivileges=yes' \
    "listener cannot gain privileges"
assert_contains "$UNIT" 'ProtectSystem=strict' \
    "whole filesystem read-only for the listener"
assert_contains "$UNIT" 'PrivateTmp=yes' \
    "listener gets a private tmp"
assert_absent "$UNIT" 'ReadWritePaths' \
    "no write paths (service is read-only by design)"

# Secrets: no committed token, no wildcard bind anywhere in the unit.
assert_absent "$UNIT" 'HEALTH_METRICS_TOKEN=' \
    "unit embeds no bearer token"
assert_absent "$UNIT" '0.0.0.0' \
    "unit never binds a wildcard address"

# Parse the unit. Substitute the not-yet-installed binary only for verification
# so systemd-analyze validates directives instead of host state.
if command -v systemd-analyze >/dev/null 2>&1; then
    mkdir -p "$TMP/system"
    while IFS= read -r line; do
        printf '%s\n' "${line//\/usr\/local\/lib\/lunarwing-health\/lunarwing-health-metrics/\/bin\/true}"
    done <"$UNIT" >"$TMP/system/lunarwing-health-metrics.service"
    if systemd-analyze verify "$TMP/system/lunarwing-health-metrics.service" >/dev/null 2>&1; then
        ok "unit passes systemd-analyze verify"
    else
        bad "unit passes systemd-analyze verify" "unit parser rejected the service file"
    fi
else
    ok "SKIP: systemd-analyze unavailable (unit parse check skipped)"
fi

# OpenRC parity: a dedicated user, no capabilities, and no new privileges.
assert_contains "$OPENRC_UNIT" 'command_user="lunarwing-metrics:lunarwing-metrics"' \
    "OpenRC listener uses its dedicated unprivileged user"
assert_contains "$OPENRC_UNIT" 'supervisor="supervise-daemon"' \
    "OpenRC listener uses the privilege-aware supervisor"
assert_contains "$OPENRC_UNIT" 'capabilities="^"' \
    "OpenRC listener has an empty capability set"
assert_contains "$OPENRC_UNIT" 'no_new_privs="yes"' \
    "OpenRC listener cannot gain privileges"
assert_contains "$OPENRC_UNIT" 'pidfile="/run/lunarwing-health-metrics/lunarwing-health-metrics.pid"' \
    "OpenRC pidfile has a dedicated runtime directory"
assert_contains "$OPENRC_UNIT" 'checkpath -d -m 0750 -o "${command_user}" /run/lunarwing-health-metrics' \
    "OpenRC runtime directory belongs to the listener"
assert_contains "$OPENRC_UNIT" 'checkpath -f -m 0640 -o "${command_user}" "${output_log}" "${error_log}"' \
    "OpenRC log files are writable by the listener"
assert_contains "$OPENRC_UNIT" 'respawn_delay="5"' \
    "OpenRC listener restarts after a failure"
assert_absent "$OPENRC_UNIT" 'command_background=' \
    "OpenRC does not bypass supervise-daemon"
assert_absent "$OPENRC_UNIT" '0.0.0.0' \
    "OpenRC unit never suggests a wildcard bind"
if sh -n "$OPENRC_UNIT"; then
    ok "OpenRC unit passes shell syntax validation"
else
    bad "OpenRC unit passes shell syntax validation" "shell parser rejected the init script"
fi

# The OpenRC installer creates the account used by the unit.
assert_contains "$INSTALLER" 'groupadd --system lunarwing-metrics' \
    "installer creates the dedicated system group"
assert_contains "$INSTALLER" 'useradd --system --no-create-home --shell /sbin/nologin \' \
    "installer creates a home-free nologin system user"
assert_contains "$INSTALLER" '--gid lunarwing-metrics lunarwing-metrics' \
    "installer assigns the dedicated primary group"

# confd contract: documented listen default stays loopback; token ships empty.
assert_contains "$CONFD" 'export HEALTH_METRICS_LISTEN="127.0.0.1:50000"' \
    "confd default listen is loopback"
assert_contains "$CONFD" 'export HEALTH_METRICS_TOKEN=""' \
    "confd ships with an empty token"
assert_absent "$CONFD" '0.0.0.0' \
    "confd never suggests a wildcard bind"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
