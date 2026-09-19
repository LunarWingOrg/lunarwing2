#!/usr/bin/env bash
# Tests for cron-wrapper.sh — the scheduled launcher for the health-check →
# metrics-export → self-heal pipeline (WI#180 F3).
#
# Self-contained: the wrapper is copied into a fake pipeline directory of stub
# scripts plus a uniquely-named mock admin tool that only exists in a test bin
# dir. Never touches the host's real health pipeline or services.
#
#   A: the wrapper's safe PATH includes the standard administrative sbin
#      directories (an rc-service under /usr/sbin must be discoverable).
#   B: the actual child environment inherits that PATH (a stub child resolves
#      a tool reachable only through LUNARWING_HEALTH_PATH).
#   C: LUNARWING_HEALTH_PATH appends an operator-supplied directory.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
CW_SRC="$DIR/../cron-wrapper.sh"

[[ -r "$CW_SRC" ]] || { echo "FATAL: cron-wrapper.sh not found at $CW_SRC"; exit 1; }

# A sandbox with a fake pipeline dir (wrapper copied in) and a uniquely-named
# mock admin tool under <sb>/sbin (never collides with host binaries).
new_sb() {
    local sb; sb="$(mktemp -d "${TMPDIR:-/tmp}/cron-wrapper-test.XXXXXX")"
    mkdir -p "$sb/pipeline" "$sb/sbin" "$sb/out"
    cp "$CW_SRC" "$sb/pipeline/cron-wrapper.sh"
    cat >"$sb/pipeline/infrastructure-health-check.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' '{"component":"stub","status":"healthy"}'
exit 0
EOF
    cat >"$sb/pipeline/health-metrics-exporter.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
    cat >"$sb/pipeline/lunarwing-self-heal.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
    chmod +x "$sb/pipeline"/*.sh
    cat >"$sb/sbin/lunarwing-test-admin-tool" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "admin tool invoked: $*"
exit 0
EOF
    chmod +x "$sb/sbin/lunarwing-test-admin-tool"
    printf '%s' "$sb"
}

# Evaluate just the wrapper's PATH assignment (never executes the pipeline).
# <wrapper> [LUNARWING_HEALTH_PATH=<dir>] -> PATH value on stdout
wrapper_path() {
    local cw="$1"; shift
    env "$@" bash -c '
        eval "$(grep "^export PATH=" "$1")"
        printf "%s\n" "$PATH"
    ' _ "$cw"
}

# run_cw <sb> [ENV=VALUE ...] -> stdout+stderr, rc via $?
run_cw() {
    local sb="$1"; shift
    env HOME="$sb/home" LUNARWING_BASE_DIR="$sb/base" "$@" bash "$sb/pipeline/cron-wrapper.sh" 2>&1
}

# ── A: safe PATH includes the sbin directories ────────────────────────────────
a="$(new_sb)"
oA="$(wrapper_path "$a/pipeline/cron-wrapper.sh")"
assert_contains "$oA" "/usr/local/sbin" "A1: safe PATH includes /usr/local/sbin"
assert_contains "$oA" "/usr/sbin"        "A2: safe PATH includes /usr/sbin"
assert_contains "$oA" "/usr/local/bin"   "A3: standard bin dirs retained"
assert_eq "$(printf '%s\n' "$oA" | awk -F: '{for(i=1;i<=NF;i++) if ($i=="/usr/local/sbin") print i}')" "4" \
    "A4: sbin dirs appended after the standard dirs (precedence preserved)"

# ── B: the child environment can invoke an operator-supplied admin tool ──────
b="$(new_sb)"
cat >"$b/pipeline/lunarwing-self-heal.sh" <<'EOF'
#!/usr/bin/env bash
command -v lunarwing-test-admin-tool >/dev/null 2>&1 || { echo "ADMIN TOOL NOT FOUND"; exit 7; }
lunarwing-test-admin-tool probe status >"$CW_TEST_OUT" 2>&1 || true
exit 0
EOF
chmod +x "$b/pipeline/lunarwing-self-heal.sh"
oB="$(run_cw "$b" LUNARWING_HEALTH_PATH="$b/sbin" CW_TEST_OUT="$b/out/call.txt")"
assert_contains "$oB" "Running self-healing watchdog" "B1: wrapper reaches self-heal"
assert_absent "$oB" "ADMIN TOOL NOT FOUND" "B2: child resolves the admin tool via the extended PATH"
assert_eq "$(cat "$b/out/call.txt" 2>/dev/null)" "admin tool invoked: probe status" \
    "B3: child actually invoked the admin tool"

# ── C: LUNARWING_HEALTH_PATH appends without disturbing the base path ────────
c="$(new_sb)"
oC="$(wrapper_path "$c/pipeline/cron-wrapper.sh" "LUNARWING_HEALTH_PATH=$c/sbin")"
assert_contains "$oC" "/usr/sbin"   "C1: base safe PATH intact with extension"
assert_eq "$(printf '%s\n' "$oC" | awk -F: '{print $NF}')" "$c/sbin" \
    "C2: operator-supplied directory appended last"

finish
