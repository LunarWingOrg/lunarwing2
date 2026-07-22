#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_SCRIPT="$TEST_DIR/../build-lunarwing.sh"

# shellcheck source=../build-lunarwing.sh
source "$BUILD_SCRIPT"

FIXTURE="$(mktemp -d "${TMPDIR:-/tmp}/build-lunarwing-test.XXXXXX")"
lock_holder_pid=""
cleanup() {
    if [[ -n "$lock_holder_pid" ]]; then
        kill "$lock_holder_pid" 2>/dev/null || true
        wait "$lock_holder_pid" 2>/dev/null || true
    fi
    rm -rf "$FIXTURE"
}
trap cleanup EXIT

failures=0
passes=0

pass() {
    printf 'PASS: %s\n' "$1"
    passes=$((passes + 1))
}

fail() {
    printf 'FAIL: %s\n      %s\n' "$1" "$2"
    failures=$((failures + 1))
}

assert_eq() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    if [[ "$actual" == "$expected" ]]; then
        pass "$label"
    else
        fail "$label" "got [$actual], expected [$expected]"
    fi
}

assert_contains() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    if grep -qF -- "$expected" <<<"$actual"; then
        pass "$label"
    else
        fail "$label" "missing [$expected]"
    fi
}

assert_not_contains() {
    local label="$1"
    local actual="$2"
    local unexpected="$3"
    if grep -qF -- "$unexpected" <<<"$actual"; then
        fail "$label" "unexpected [$unexpected]"
    else
        pass "$label"
    fi
}

assert_file_contains() {
    local label="$1"
    local file="$2"
    local expected="$3"
    if grep -qF -- "$expected" "$file"; then
        pass "$label"
    else
        fail "$label" "missing [$expected] in $file"
    fi
}

printf '%s\n' '=== automatic job calculation ==='

CPU_COUNT=16
MEMORY_KNOWN=true
MEM_AVAILABLE_MB=7108
MEMORY_PER_JOB_MB=1536
export CPU_COUNT MEMORY_KNOWN MEM_AVAILABLE_MB MEMORY_PER_JOB_MB
calculate_recommended_jobs
assert_eq '75% of 16 CPUs is 12' "$CPU_JOB_LIMIT" '12'
assert_eq '7108MB permits four automatic jobs' "$MEMORY_JOB_LIMIT" '4'
assert_eq 'memory caps the recommendation' "$RECOMMENDED_JOBS" '4'

CPU_COUNT=1
MEMORY_KNOWN=true
MEM_AVAILABLE_MB=32768
calculate_recommended_jobs
assert_eq 'single CPU never produces zero jobs' "$RECOMMENDED_JOBS" '1'

CPU_COUNT=8
MEMORY_KNOWN=true
MEM_AVAILABLE_MB=512
calculate_recommended_jobs
assert_eq 'insufficient memory produces no automatic jobs' "$RECOMMENDED_JOBS" '0'

JOBS=""
JOBS_SOURCE='auto'
export JOBS JOBS_SOURCE
set +e
low_memory_output="$(select_jobs 2>&1)"
low_memory_rc=$?
set -e
if [[ "$low_memory_rc" -ne 0 ]]; then
    pass 'automatic build fails below the per-job memory floor'
else
    fail 'automatic build fails below the per-job memory floor' 'selection unexpectedly succeeded'
fi
assert_contains 'low-memory failure suggests an explicit override' "$low_memory_output" 'explicitly override with -j 1'

JOBS=1
JOBS_SOURCE='command line'
explicit_low_memory_output="$(select_jobs 2>&1)"
assert_contains 'explicit low-memory override is warned and honored' "$explicit_low_memory_output" 'explicit -j1 will be attempted'
JOBS=""
JOBS_SOURCE='auto'

CPU_COUNT=8
MEMORY_KNOWN=false
calculate_recommended_jobs
assert_eq 'unknown memory uses the CPU recommendation' "$RECOMMENDED_JOBS" '6'

printf '%s\n' '=== inherited cgroup limits ==='

CGROUP_FIXTURE="$FIXTURE/cgroup"
mkdir -p "$CGROUP_FIXTURE/parent/leaf"
printf '%s\n' 'max 100000' > "$CGROUP_FIXTURE/cpu.max"
printf '%s\n' '200000 100000' > "$CGROUP_FIXTURE/parent/cpu.max"
printf '%s\n' 'max 100000' > "$CGROUP_FIXTURE/parent/leaf/cpu.max"
printf '%s\n' '8589934592' > "$CGROUP_FIXTURE/memory.max"
printf '%s\n' '3221225472' > "$CGROUP_FIXTURE/memory.current"
printf '%s\n' '4294967296' > "$CGROUP_FIXTURE/parent/memory.max"
printf '%s\n' '2147483648' > "$CGROUP_FIXTURE/parent/memory.current"
printf '%s\n' 'max' > "$CGROUP_FIXTURE/parent/leaf/memory.max"
printf '%s\n' '104857600' > "$CGROUP_FIXTURE/parent/leaf/memory.current"

CGROUP_VERSION=2
CGROUP_CPU_ROOT="$CGROUP_FIXTURE"
CGROUP_CPU_DIR="$CGROUP_FIXTURE/parent/leaf"
CGROUP_MEMORY_ROOT="$CGROUP_FIXTURE"
CGROUP_MEMORY_DIR="$CGROUP_FIXTURE/parent/leaf"
export CGROUP_VERSION CGROUP_CPU_ROOT CGROUP_CPU_DIR CGROUP_MEMORY_ROOT CGROUP_MEMORY_DIR
assert_eq 'ancestor CPU quota is applied' "$(cgroup_cpu_count)" '2'
assert_eq 'most restrictive ancestor memory headroom is applied' "$(cgroup_memory_available_mb)" '2048'

printf '%s\n' '=== platform and safety validation ==='

set +e
platform_output="$( {
    uname() {
        case "$1" in
            -s) printf '%s\n' 'Darwin' ;;
            -m) printf '%s\n' 'arm64' ;;
        esac
    }
    detect_platform
} 2>&1)"
platform_rc=$?
set -e
if [[ "$platform_rc" -ne 0 ]]; then
    pass 'non-Linux hosts are rejected'
else
    fail 'non-Linux hosts are rejected' 'Darwin was accepted'
fi
assert_contains 'Linux-only error is explicit' "$platform_output" 'supports Linux only'

aarch_output="$( {
    uname() {
        case "$1" in
            -s) printf '%s\n' 'Linux' ;;
            -m) printf '%s\n' 'aarch64' ;;
        esac
    }
    detect_platform
    printf '%s:%s\n' "$OS" "$ARCH"
} 2>&1)"
assert_eq 'Linux aarch64 uses the supported shared path' "$aarch_output" 'Linux:aarch64'

if grep -Eq '(^|[^[:alnum:]_])(pkill|killall|eval)([^[:alnum:]_]|$)' "$BUILD_SCRIPT"; then
    fail 'destructive and eval commands are absent' 'found pkill, killall, or eval'
else
    pass 'destructive and eval commands are absent'
fi

printf '%s\n' '=== mocked end-to-end invocation ==='

mkdir -p "$FIXTURE/bin" "$FIXTURE/repo/ic" "$FIXTURE/logs"
touch "$FIXTURE/repo/ic/Cargo.toml" "$FIXTURE/repo/ic/Cargo.lock"

cat > "$FIXTURE/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == '--version' ]]; then
    printf '%s\n' 'cargo 1.92.0 (fixture)'
    exit 0
fi

{
    printf 'COMMAND'
    printf ' %q' "$@"
    printf '\n'
    for arg in "$@"; do
        printf 'ARG=%s\n' "$arg"
    done
} >> "${FAKE_CARGO_LOG:?}"
if [[ "${1:-}" == 'build' ]]; then
    if [[ "${FAKE_CARGO_FAIL_BUILD:-0}" == '1' ]]; then
        printf '%s\n' 'fixture build failure' >&2
        exit 42
    fi
    profile='debug'
    for arg in "$@"; do
        [[ "$arg" == '--release' ]] && profile='release'
    done
    artifact_root="$CARGO_TARGET_DIR"
    if [[ -n "${CARGO_BUILD_TARGET:-}" ]]; then
        artifact_root="$artifact_root/$CARGO_BUILD_TARGET"
    fi
    mkdir -p "$artifact_root/$profile"
    printf '#!/usr/bin/env bash\n' > "$artifact_root/$profile/lunarwing"
    chmod +x "$artifact_root/$profile/lunarwing"
    printf '%s\n' '   Compiling lunarwing-fixture v0.0.0'
fi
EOF

cat > "$FIXTURE/bin/rustc" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'rustc 1.92.0 (fixture)'
EOF

chmod +x "$FIXTURE/bin/cargo" "$FIXTURE/bin/rustc"
FAKE_CARGO_LOG="$FIXTURE/cargo-args"
export FAKE_CARGO_LOG
SYSTEM_PATH="$PATH"

run_fixture() {
    env \
        PATH="$FIXTURE/bin:$SYSTEM_PATH" \
        LUNARWING_REPO="$FIXTURE/repo" \
        CARGO_TARGET_DIR="${TEST_TARGET_DIR:-$FIXTURE/new-target}" \
        CARGO_BUILD_TARGET="${TEST_CARGO_BUILD_TARGET:-}" \
        BUILD_LOG_DIR="${TEST_LOG_DIR:-$FIXTURE/logs}" \
        BUILD_JOBS="${TEST_BUILD_JOBS:-2}" \
        FAKE_CARGO_FAIL_BUILD="${TEST_FAIL_BUILD:-0}" \
        RUSTC_WRAPPER='fixture-wrapper' \
        "$BUILD_SCRIPT" "$@"
}

fixture_output="$(run_fixture -j 3 2>&1)"
assert_file_contains \
    'Cargo receives a locked release binary build' \
    "$FAKE_CARGO_LOG" \
    'COMMAND build --locked --bin lunarwing -j 3 --release'
assert_file_contains 'Cargo receives jobs as a separate argument' "$FAKE_CARGO_LOG" 'ARG=-j'
if [[ -d "$FIXTURE/new-target" ]]; then
    pass 'missing target directory is created'
else
    fail 'missing target directory is created' "$FIXTURE/new-target is absent"
fi
assert_contains 'command-line jobs override BUILD_JOBS' "$fixture_output" 'Selected jobs: 3 (command line)'

ln -s / "$FIXTURE/root-target-link"
set +e
root_target_output="$(TEST_TARGET_DIR="$FIXTURE/root-target-link" run_fixture --dry-run 2>&1)"
root_target_rc=$?
set -e
if [[ "$root_target_rc" -eq 2 ]]; then
    pass 'target symlinks resolving to the filesystem root are rejected'
else
    fail 'target symlinks resolving to the filesystem root are rejected' "exit=$root_target_rc"
fi
assert_contains 'root target rejection is explicit' "$root_target_output" 'filesystem root cannot be used as CARGO_TARGET_DIR'

: > "$FAKE_CARGO_LOG"
dry_output="$(run_fixture --dry-run 2>&1)"
if grep -q '^COMMAND build ' "$FAKE_CARGO_LOG"; then
    fail 'dry run does not invoke cargo build' 'a build invocation was recorded'
else
    pass 'dry run does not invoke cargo build'
fi
assert_contains 'dry run prints validated command' "$dry_output" 'cargo build --locked --bin lunarwing -j 2 --release'

space_target="$FIXTURE/target with spaces"
space_log_dir="$FIXTURE/logs with spaces"
space_output="$(TEST_TARGET_DIR="$space_target" TEST_LOG_DIR="$space_log_dir" run_fixture -j 1 2>&1)"
if [[ -x "$space_target/release/lunarwing" ]]; then
    pass 'target and log paths containing spaces are preserved'
else
    fail 'target and log paths containing spaces are preserved' 'fixture binary is absent'
fi
assert_contains 'spaced binary path is reported' "$space_output" "Binary: $space_target/release/lunarwing"

configured_target="$FIXTURE/configured-target"
configured_output="$(
    TEST_TARGET_DIR="$configured_target" \
    TEST_CARGO_BUILD_TARGET='aarch64-unknown-linux-gnu' \
    run_fixture -j 1 2>&1
)"
assert_contains \
    'configured Cargo target binary is discovered' \
    "$configured_output" \
    "$configured_target/aarch64-unknown-linux-gnu/release/lunarwing"
assert_not_contains \
    'configured Cargo target does not produce a false warning' \
    "$configured_output" \
    'no executable LunarWing binary was found'

set +e
failure_output="$(
    TEST_TARGET_DIR="$FIXTURE/failing-target" \
    TEST_FAIL_BUILD=1 \
    run_fixture -j 1 --verbose 2>&1
)"
failure_rc=$?
set -e
if [[ "$failure_rc" -eq 42 ]]; then
    pass 'verbose Cargo pipeline preserves the build failure status'
else
    fail 'verbose Cargo pipeline preserves the build failure status' "exit=$failure_rc"
fi
assert_contains 'failed build output is retained' "$failure_output" 'fixture build failure'

if command -v flock >/dev/null 2>&1; then
    locked_target="$FIXTURE/locked-target"
    mkdir -p "$locked_target"
    lock_file="$locked_target.lunarwing-build.lock"
    lock_ready="$FIXTURE/lock-ready"
    (
        exec 9>> "$lock_file"
        flock 9
        touch "$lock_ready"
        sleep 30
    ) &
    lock_holder_pid=$!
    for _ in {1..100}; do
        [[ -e "$lock_ready" ]] && break
        sleep 0.02
    done

    set +e
    lock_output="$(TEST_TARGET_DIR="$locked_target/../locked-target/." run_fixture --clean 2>&1)"
    lock_rc=$?
    set -e
    kill "$lock_holder_pid" 2>/dev/null || true
    wait "$lock_holder_pid" 2>/dev/null || true
    lock_holder_pid=""

    if [[ "$lock_rc" -ne 0 ]]; then
        pass 'concurrent clean/build use of a target is rejected'
    else
        fail 'concurrent clean/build use of a target is rejected' 'contended --clean succeeded'
    fi
    assert_contains 'lock contention error identifies the target' "$lock_output" 'Another build-lunarwing process is using target directory'
else
    pass 'flock contention test skipped because flock is unavailable'
fi

injection_marker="$FIXTURE/injected"
set +e
invalid_output="$(TEST_BUILD_JOBS="1;touch $injection_marker" run_fixture 2>&1)"
invalid_rc=$?
set -e
if [[ "$invalid_rc" -eq 2 && ! -e "$injection_marker" ]]; then
    pass 'malicious BUILD_JOBS is rejected without execution'
else
    fail 'malicious BUILD_JOBS is rejected without execution' "exit=$invalid_rc marker=$([[ -e "$injection_marker" ]] && printf yes || printf no)"
fi
assert_contains 'invalid jobs error is actionable' "$invalid_output" 'Build jobs must be a positive integer'

set +e
missing_output="$(run_fixture --jobs 2>&1)"
missing_rc=$?
set -e
if [[ "$missing_rc" -eq 2 ]]; then
    pass 'missing option values return usage error status'
else
    fail 'missing option values return usage error status' "exit=$missing_rc"
fi
assert_contains 'missing jobs value is explained' "$missing_output" '--jobs requires a value'

set +e
profile_output="$(run_fixture --profile optimized 2>&1)"
profile_rc=$?
set -e
if [[ "$profile_rc" -eq 2 ]]; then
    pass 'invalid profiles return usage error status'
else
    fail 'invalid profiles return usage error status' "exit=$profile_rc"
fi
assert_contains 'invalid profile is explained' "$profile_output" "Invalid profile 'optimized'"

printf '\n%d passed, %d failed\n' "$passes" "$failures"
[[ "$failures" -eq 0 ]]
