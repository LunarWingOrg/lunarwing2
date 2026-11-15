#!/usr/bin/env bash
# Static + systemd-analyze validation of the healthpoint metrics listener's
# systemd unit (ic/systemd/lunarwing-health-metrics.service).
#
# The listener is loopback-only by construction in the binary; the unit must
# keep that contract: read-only filesystem, dynamic user, optional env file,
# no committed tokens, no write paths. systemd-analyze verify runs only when
# the tool exists (this suite is also green on OpenRC-only hosts).
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
UNIT="$ROOT_DIR/ic/systemd/lunarwing-health-metrics.service"
CONFD="$ROOT_DIR/ic/systemd/lunarwing-health-metrics.confd"

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

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
