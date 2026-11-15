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
listener_pid=""
trap 'if [[ -n "$listener_pid" ]]; then kill "$listener_pid" 2>/dev/null || true; fi; rm -rf "$TMP"' EXIT

pass=0
fail=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }

assert_contains() {
    local file="$1" needle="$2" label="$3"
    if grep -qF -- "$needle" "$file"; then
        ok "$label"
    else
        bad "$label" "missing: $needle"
    fi
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

# OpenRC parity: a dedicated user, no capabilities, no new privileges, and a
# pid file that only root can reach.
assert_contains "$OPENRC_UNIT" 'command_user="lunarwing-metrics:lunarwing-metrics"' \
    "OpenRC listener uses its dedicated unprivileged user"
assert_contains "$OPENRC_UNIT" 'supervisor="supervise-daemon"' \
    "OpenRC listener uses the privilege-aware supervisor"
assert_contains "$OPENRC_UNIT" 'capabilities="^"' \
    "OpenRC listener has an empty capability set"
assert_contains "$OPENRC_UNIT" 'no_new_privs="yes"' \
    "OpenRC listener cannot gain privileges"
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

# The root supervisor creates the pid file at start and signals the pid it
# names at stop, so nothing on that path may belong to the service account:
# an account that can replace the file can make root kill or overwrite.
pidfile="$(sed -n 's/^pidfile="\([^"]*\)"$/\1/p' "$OPENRC_UNIT")"
pidfile_dir="$(dirname "${pidfile:-.}")"
if [[ "$pidfile" == "/run/lunarwing-health-metrics.pid" ]]; then
    ok "OpenRC pidfile lives directly in root-owned /run"
else
    bad "OpenRC pidfile lives directly in root-owned /run" "found: ${pidfile:-none}"
fi
if grep -E '^[[:space:]]*checkpath' "$OPENRC_UNIT" | grep -qF -- "${pidfile_dir}/"; then
    bad "no account-owned directory on the pid file path" \
        "a checkpath line names ${pidfile_dir}/"
else
    ok "no account-owned directory on the pid file path"
fi
assert_absent "$OPENRC_UNIT" 'checkpath -d' \
    "OpenRC creates no account-owned runtime directory"

# The OpenRC installer creates the account used by the unit, and keeps an
# operator's existing conf.d instead of resetting port, token and base dir.
assert_contains "$INSTALLER" 'groupadd --system lunarwing-metrics' \
    "installer creates the dedicated system group"
assert_contains "$INSTALLER" 'useradd --system --no-create-home --home-dir /var/empty \' \
    "installer creates a home-free nologin system user"
assert_contains "$INSTALLER" '--gid lunarwing-metrics lunarwing-metrics' \
    "installer assigns the dedicated primary group"
if awk '/^install_openrc_confd\(\) \{/,/^\}/' "$INSTALLER" \
    | grep -qF '[[ -e "$OPENRC_CONFD" ]] ||'; then
    ok "installer installs conf.d only when none exists"
else
    bad "installer installs conf.d only when none exists" \
        "conf.d guard missing from install_openrc_confd"
fi

# Exercise that helper against an installed conf.d. A second installer run is
# the upgrade path, so the operator's settings must survive it untouched.
CONFD_HELPER="$TMP/install-openrc-confd.sh"
{
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    printf 'install() {\n'
    printf '  local src="" dest=""\n'
    printf '  while [[ $# -gt 0 ]]; do\n'
    printf '    case "$1" in -o|-g|-m) shift 2 ;; *) src="$dest"; dest="$1"; shift ;; esac\n'
    printf '  done\n'
    printf '  cp -- "$src" "$dest"\n'
    printf '}\n'
    printf 'IC_DIR=%q\nOPENRC_CONFD=$1\n' "$ROOT_DIR/ic"
    awk '/^install_openrc_confd\(\) \{/,/^\}/' "$INSTALLER"
    printf 'install_openrc_confd\n'
} >"$CONFD_HELPER"
mkdir -p "$TMP/kept" "$TMP/fresh"
KEPT_CONFD="$TMP/kept/lunarwing-health-metrics"
cat >"$KEPT_CONFD" <<'EOF'
export HEALTH_METRICS_LISTEN="127.0.0.1:50123"
export HEALTH_METRICS_TOKEN="keep-token"
EOF
chmod 0640 "$KEPT_CONFD"
cp "$KEPT_CONFD" "$TMP/kept-reference"
if bash "$CONFD_HELPER" "$KEPT_CONFD"; then
    if cmp -s "$KEPT_CONFD" "$TMP/kept-reference"; then
        ok "re-running the installer keeps the operator's conf.d"
    else
        bad "re-running the installer keeps the operator's conf.d" "file content changed"
    fi
    if [[ "$(stat -c '%a' "$KEPT_CONFD" 2>/dev/null || stat -f '%Lp' "$KEPT_CONFD")" == 640 ]]; then
        ok "an existing conf.d keeps its permissions"
    else
        bad "an existing conf.d keeps its permissions" "expected mode 640"
    fi
else
    bad "re-running the installer keeps the operator's conf.d" "helper returned non-zero"
fi
FRESH_CONFD="$TMP/fresh/lunarwing-health-metrics"
if bash "$CONFD_HELPER" "$FRESH_CONFD" && [[ -f "$FRESH_CONFD" ]]; then
    ok "a fresh install writes the committed conf.d defaults"
else
    bad "a fresh install writes the committed conf.d defaults" "conf.d was not created"
fi

# confd contract: documented listen default stays loopback; token ships empty.
assert_contains "$CONFD" 'export HEALTH_METRICS_LISTEN="127.0.0.1:50000"' \
    "confd default listen is loopback"
assert_contains "$CONFD" 'export HEALTH_METRICS_TOKEN=""' \
    "confd ships with an empty token"
assert_absent "$CONFD" '0.0.0.0' \
    "confd never suggests a wildcard bind"
assert_contains "$CONFD" 'export LUNARWING_BASE_DIR="/var/lib/lunarwing-health"' \
    "confd exports the exporter base directory"

PIPELINE="$ROOT_DIR/ic/scripts/mt-admin-lib/12-health-pipeline.sh"
assert_contains "$PIPELINE" '  health_metrics_confd_reconcile' \
    "health pipeline reconciles the OpenRC listener confd"

# Exercise the shipped reconciler against an installed confd. The listener is
# optional, so this fixture explicitly provides the file the helper manages.
RECONCILE_CONFD="$TMP/lunarwing-health-metrics"
RECONCILE_SCRIPT="$TMP/reconcile-confd.sh"
cat >"$RECONCILE_CONFD" <<'EOF'
# Operator-owned listener settings must survive reconciliation.
export HEALTH_METRICS_LISTEN="127.0.0.1:50123"
export LUNARWING_BASE_DIR="/old/base"
export HEALTH_METRICS_TOKEN="keep-token"
export LUNARWING_BASE_DIR="/duplicate/base"
EOF
chmod 0640 "$RECONCILE_CONFD"
{
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    printf 'say() { :; }\nINIT_SYSTEM=openrc\n'
    printf 'OPENRC_CONFD_DIR=%q\nHEALTH_BASE_DIR=%q\n' "$TMP" "$TMP/custom-base"
    awk '/^health_metrics_confd_reconcile\(\) \{/,/^\}/' "$PIPELINE"
    printf 'health_metrics_confd_reconcile\nhealth_metrics_confd_reconcile\n'
} >"$RECONCILE_SCRIPT"
if bash "$RECONCILE_SCRIPT"; then
    assert_contains "$RECONCILE_CONFD" "export LUNARWING_BASE_DIR=\"$TMP/custom-base\"" \
        "reconciler applies an overridden health base directory"
    assert_absent "$RECONCILE_CONFD" '/old/base' \
        "reconciler removes the stale base directory"
    assert_absent "$RECONCILE_CONFD" '/duplicate/base' \
        "reconciler removes duplicate base assignments"
    assert_contains "$RECONCILE_CONFD" 'export HEALTH_METRICS_TOKEN="keep-token"' \
        "reconciler preserves the operator token"
    assert_contains "$RECONCILE_CONFD" 'export HEALTH_METRICS_LISTEN="127.0.0.1:50123"' \
        "reconciler preserves the operator listen address"
    if [[ "$(grep -Ec '^[[:space:]]*(export[[:space:]]+)?LUNARWING_BASE_DIR=' "$RECONCILE_CONFD")" == 1 ]]; then
        ok "reconciler is idempotent and leaves one base assignment"
    else
        bad "reconciler is idempotent and leaves one base assignment" "unexpected assignment count"
    fi
    if [[ "$(stat -c '%a' "$RECONCILE_CONFD")" == 640 ]]; then
        ok "reconciler preserves confd permissions"
    else
        bad "reconciler preserves confd permissions" "expected mode 640"
    fi
else
    bad "OpenRC confd reconciler runs" "fixture returned non-zero"
fi

# Acceptance check for WI#700: with no inherited base-dir or output override,
# the confd alone must send the listener to the exporter's report (200, not
# the permanent 503 produced by root's default HOME fallback).
METRICS_BIN="${LUNARWING_HEALTH_METRICS_BIN:-$ROOT_DIR/ic/target/release/lunarwing-health-metrics}"
if [[ ! -x "$METRICS_BIN" ]]; then
    ok "SKIP: live confd scrape (set LUNARWING_HEALTH_METRICS_BIN to the listener binary)"
elif ! command -v curl >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
    ok "SKIP: live confd scrape (curl and python3 are required)"
else
    live_base="$TMP/live-base"
    live_confd="$TMP/live.confd"
    live_output="$TMP/live.metrics"
    live_log="$TMP/live.log"
    mkdir -p "$live_base/workspace/reports/health"
    printf 'lunarwing_health_overall_status 0\n' >"$live_base/workspace/reports/health/metrics.prom"
    live_port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
    sed -e "s/^export HEALTH_METRICS_LISTEN=.*/export HEALTH_METRICS_LISTEN=\"127.0.0.1:${live_port}\"/" \
        -e "s|^export LUNARWING_BASE_DIR=.*|export LUNARWING_BASE_DIR=\"${live_base}\"|" \
        "$CONFD" >"$live_confd"
    # shellcheck disable=SC2016  # $1/$2 are intentionally expanded by the child shell.
    env -i HOME=/root PATH="$PATH" bash -c '
        set -a
        . "$1"
        set +a
        exec "$2"
    ' _ "$live_confd" "$METRICS_BIN" >"$live_log" 2>&1 &
    listener_pid=$!
    live_code=""
    for _ in {1..50}; do
        live_code="$(curl -sS --noproxy '*' --max-time 1 -o "$live_output" -w '%{http_code}' \
            "http://127.0.0.1:${live_port}/metrics" 2>/dev/null || true)"
        [[ "$live_code" == 200 ]] && break
        kill -0 "$listener_pid" 2>/dev/null || break
        sleep 0.1
    done
    if [[ "$live_code" == 200 ]]; then
        ok "confd-only listener serves the exporter report with HTTP 200"
    else
        bad "confd-only listener serves the exporter report with HTTP 200" \
            "HTTP ${live_code:-unavailable}; $(tail -n 2 "$live_log" 2>/dev/null || true)"
    fi
    if grep -qF 'lunarwing_health_overall_status 0' "$live_output" 2>/dev/null; then
        ok "confd-only listener returns the configured report body"
    else
        bad "confd-only listener returns the configured report body" "report marker was absent"
    fi
    kill "$listener_pid" 2>/dev/null || true
    wait "$listener_pid" 2>/dev/null || true
    listener_pid=""
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
