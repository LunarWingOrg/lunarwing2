#!/usr/bin/env bash
# Unified native LunarWing build helper for Linux aarch64 and x86_64 hosts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INVOCATION_DIR="$PWD"
REPO_ROOT="${LUNARWING_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target}"
BUILD_LOG_DIR="${BUILD_LOG_DIR:-${TMPDIR:-/tmp}}"
MEMORY_PER_JOB_MB="${BUILD_MEMORY_PER_JOB_MB:-1536}"

IC_DIR=""
PROFILE="release"
JOBS="${BUILD_JOBS:-}"
JOBS_SOURCE="auto"
[[ -n "$JOBS" ]] && JOBS_SOURCE="BUILD_JOBS"

DO_CLEAN=false
DO_WASM=false
DRY_RUN=false
VERBOSE=false

OS=""
ARCH=""
CPU_COUNT=1
CPU_JOB_LIMIT=1
MEM_TOTAL_MB=0
MEM_AVAILABLE_MB=0
MEMORY_KNOWN=false
MEMORY_JOB_LIMIT=1
RECOMMENDED_JOBS=1
CGROUP_MEMORY_DIR=""
CGROUP_CPU_DIR=""
CGROUP_VERSION=""
CGROUP_MEMORY_ROOT=""
CGROUP_CPU_ROOT=""
BUILD_LOCK_FD=""
BUILD_LOCK_FILE=""

log_info() { printf '[INFO]  %s\n' "$*"; }
log_ok() { printf '[OK]    %s\n' "$*"; }
log_warn() { printf '[WARN]  %s\n' "$*" >&2; }
log_error() { printf '[ERROR] %s\n' "$*" >&2; }

die() {
    local message="$1"
    local exit_code="${2:-1}"
    log_error "$message"
    exit "$exit_code"
}

usage() {
    cat <<'EOF'
build-lunarwing.sh - build LunarWing natively on Linux

Usage:
  ./scripts/build-lunarwing.sh [OPTIONS]

Options:
  -c, --clean         Run cargo clean before building
  -j, --jobs N        Override auto-detected parallel jobs
  -t, --target DIR    Override CARGO_TARGET_DIR
  -r, --repo DIR      Override the repository root
      --profile MODE  Build profile: release (default) or debug
      --wasm          Compatibility no-op; WASM artifacts build elsewhere
      --dry-run       Print the validated build command without running it
  -v, --verbose       Stream Cargo output while retaining the build log
  -h, --help          Show this help message

Environment defaults (command-line flags take precedence):
  CARGO_TARGET_DIR        Build artifact directory
  LUNARWING_REPO          Repository root
  BUILD_JOBS              Positive integer job override
  BUILD_MEMORY_PER_JOB_MB Memory reserved per automatic job (default: 1536)
  BUILD_LOG_DIR           Build log directory (default: TMPDIR or /tmp)

Automatic jobs are the smaller of:
  - 75% of effective CPUs, with a minimum of one
  - available memory divided by BUILD_MEMORY_PER_JOB_MB

Automatic selection stops if memory cannot support one job. An explicit -j
override is allowed with a warning. Real builds take a non-blocking sidecar
lock for the canonical target directory; --clean requires flock.

Examples:
  ./scripts/build-lunarwing.sh
  ./scripts/build-lunarwing.sh --clean
  ./scripts/build-lunarwing.sh -j 4
  BUILD_JOBS=2 ./scripts/build-lunarwing.sh --profile release
EOF
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -c|--clean)
                DO_CLEAN=true
                shift
                ;;
            -j|--jobs)
                [[ $# -ge 2 ]] || die "$1 requires a value" 2
                JOBS="$2"
                JOBS_SOURCE="command line"
                shift 2
                ;;
            -t|--target)
                [[ $# -ge 2 ]] || die "$1 requires a value" 2
                TARGET_DIR="$2"
                shift 2
                ;;
            -r|--repo)
                [[ $# -ge 2 ]] || die "$1 requires a value" 2
                REPO_ROOT="$2"
                shift 2
                ;;
            --profile)
                [[ $# -ge 2 ]] || die "$1 requires a value" 2
                PROFILE="$2"
                shift 2
                ;;
            --wasm)
                DO_WASM=true
                shift
                ;;
            --dry-run)
                DRY_RUN=true
                shift
                ;;
            -v|--verbose)
                VERBOSE=true
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                usage >&2
                exit 2
                ;;
        esac
    done
}

is_positive_integer() {
    [[ "$1" =~ ^[0-9]+$ ]] && ((10#$1 > 0))
}

make_absolute() {
    local path="$1"
    if [[ "$path" == /* ]]; then
        printf '%s\n' "$path"
    else
        printf '%s/%s\n' "$INVOCATION_DIR" "$path"
    fi
}

validate_config() {
    case "$PROFILE" in
        release|debug) ;;
        *) die "Invalid profile '$PROFILE'; expected release or debug" 2 ;;
    esac

    [[ -n "$REPO_ROOT" ]] || die "Repository path cannot be empty" 2
    [[ -n "$TARGET_DIR" ]] || die "Target directory cannot be empty" 2
    [[ -n "$BUILD_LOG_DIR" ]] || die "Build log directory cannot be empty" 2
    is_positive_integer "$MEMORY_PER_JOB_MB" || \
        die "BUILD_MEMORY_PER_JOB_MB must be a positive integer" 2
    MEMORY_PER_JOB_MB=$((10#$MEMORY_PER_JOB_MB))

    if [[ -n "$JOBS" ]]; then
        is_positive_integer "$JOBS" || \
            die "Build jobs must be a positive integer, got '$JOBS'" 2
        JOBS=$((10#$JOBS))
    fi

    REPO_ROOT="$(make_absolute "$REPO_ROOT")"
    TARGET_DIR="$(make_absolute "$TARGET_DIR")"
    BUILD_LOG_DIR="$(make_absolute "$BUILD_LOG_DIR")"
    command -v realpath >/dev/null 2>&1 || die "realpath is required to validate CARGO_TARGET_DIR"
    TARGET_DIR="$(realpath -m -- "$TARGET_DIR")" || die "Unable to resolve target directory: $TARGET_DIR"
    [[ "$TARGET_DIR" != "/" ]] || die "The filesystem root cannot be used as CARGO_TARGET_DIR" 2
    IC_DIR="$REPO_ROOT/ic"
}

detect_platform() {
    OS="$(uname -s 2>/dev/null || true)"
    [[ "$OS" == "Linux" ]] || \
        die "Unsupported operating system '${OS:-unknown}'; this script supports Linux only"

    ARCH="$(uname -m 2>/dev/null || true)"
    case "$ARCH" in
        x86_64|amd64|aarch64|arm64) ;;
        *) die "Unsupported Linux architecture '${ARCH:-unknown}'; expected aarch64 or x86_64" ;;
    esac
}

detect_cgroup_paths() {
    local relative_path=""
    local candidate=""
    local controller_root=""

    CGROUP_MEMORY_DIR=""
    CGROUP_CPU_DIR=""
    CGROUP_VERSION=""
    CGROUP_MEMORY_ROOT=""
    CGROUP_CPU_ROOT=""

    if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
        relative_path="$(awk -F: '$1 == "0" {print $3; exit}' /proc/self/cgroup 2>/dev/null || true)"
        candidate="/sys/fs/cgroup${relative_path:-/}"
        if [[ -d "$candidate" ]]; then
            CGROUP_VERSION="2"
            CGROUP_MEMORY_DIR="$candidate"
            CGROUP_CPU_DIR="$candidate"
            CGROUP_MEMORY_ROOT="/sys/fs/cgroup"
            CGROUP_CPU_ROOT="/sys/fs/cgroup"
        fi
        return 0
    fi

    relative_path="$(awk -F: '$2 ~ /(^|,)memory(,|$)/ {print $3; exit}' /proc/self/cgroup 2>/dev/null || true)"
    candidate="/sys/fs/cgroup/memory${relative_path:-/}"
    if [[ -d "$candidate" ]]; then
        CGROUP_VERSION="1"
        CGROUP_MEMORY_DIR="$candidate"
        CGROUP_MEMORY_ROOT="/sys/fs/cgroup/memory"
    fi

    relative_path="$(awk -F: '$2 ~ /(^|,)cpu(,|$)/ {print $3; exit}' /proc/self/cgroup 2>/dev/null || true)"
    for controller_root in /sys/fs/cgroup/cpu /sys/fs/cgroup/cpu,cpuacct; do
        candidate="$controller_root${relative_path:-/}"
        if [[ -d "$candidate" ]]; then
            CGROUP_VERSION="1"
            CGROUP_CPU_DIR="$candidate"
            CGROUP_CPU_ROOT="$controller_root"
            break
        fi
    done
}

cgroup_cpu_count() {
    local current_dir="$CGROUP_CPU_DIR"
    local quota=""
    local period=""
    local quota_count=0
    local minimum_count=""

    [[ -n "$current_dir" && -n "$CGROUP_CPU_ROOT" ]] || return 0

    while [[ "$current_dir" == "$CGROUP_CPU_ROOT" || "$current_dir" == "$CGROUP_CPU_ROOT/"* ]]; do
        quota=""
        period=""
        if [[ "$CGROUP_VERSION" == "2" && -r "$current_dir/cpu.max" ]]; then
            read -r quota period < "$current_dir/cpu.max" || true
        elif [[ "$CGROUP_VERSION" == "1" \
            && -r "$current_dir/cpu.cfs_quota_us" \
            && -r "$current_dir/cpu.cfs_period_us" ]]; then
            quota="$(<"$current_dir/cpu.cfs_quota_us")"
            period="$(<"$current_dir/cpu.cfs_period_us")"
        fi

        if [[ "$quota" =~ ^[0-9]+$ && "$period" =~ ^[0-9]+$ ]] \
            && ((quota > 0 && period > 0)); then
            quota_count=$(((quota + period - 1) / period))
            if [[ -z "$minimum_count" ]] || ((quota_count < minimum_count)); then
                minimum_count="$quota_count"
            fi
        fi

        [[ "$current_dir" == "$CGROUP_CPU_ROOT" ]] && break
        current_dir="${current_dir%/*}"
    done

    if [[ -n "$minimum_count" ]]; then
        printf '%s\n' "$minimum_count"
    fi
}

cgroup_memory_available_mb() {
    local current_dir="$CGROUP_MEMORY_DIR"
    local limit_bytes=""
    local current_bytes=""
    local remaining_bytes=0
    local minimum_bytes=""

    [[ -n "$current_dir" && -n "$CGROUP_MEMORY_ROOT" ]] || return 0

    while [[ "$current_dir" == "$CGROUP_MEMORY_ROOT" || "$current_dir" == "$CGROUP_MEMORY_ROOT/"* ]]; do
        limit_bytes=""
        current_bytes=""
        if [[ "$CGROUP_VERSION" == "2" \
            && -r "$current_dir/memory.max" \
            && -r "$current_dir/memory.current" ]]; then
            limit_bytes="$(<"$current_dir/memory.max")"
            current_bytes="$(<"$current_dir/memory.current")"
        elif [[ "$CGROUP_VERSION" == "1" \
            && -r "$current_dir/memory.limit_in_bytes" \
            && -r "$current_dir/memory.usage_in_bytes" ]]; then
            limit_bytes="$(<"$current_dir/memory.limit_in_bytes")"
            current_bytes="$(<"$current_dir/memory.usage_in_bytes")"
        fi

        if [[ "$limit_bytes" =~ ^[0-9]+$ && "$current_bytes" =~ ^[0-9]+$ ]]; then
            remaining_bytes=0
            if ((limit_bytes > current_bytes)); then
                remaining_bytes=$((limit_bytes - current_bytes))
            fi
            if [[ -z "$minimum_bytes" ]] || ((remaining_bytes < minimum_bytes)); then
                minimum_bytes="$remaining_bytes"
            fi
        fi

        [[ "$current_dir" == "$CGROUP_MEMORY_ROOT" ]] && break
        current_dir="${current_dir%/*}"
    done

    if [[ -n "$minimum_bytes" ]]; then
        printf '%s\n' "$((minimum_bytes / 1024 / 1024))"
    fi
}

detect_cpu_count() {
    local detected=""
    local quota_count=""

    if command -v nproc >/dev/null 2>&1; then
        detected="$(nproc 2>/dev/null || true)"
    elif command -v getconf >/dev/null 2>&1; then
        detected="$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)"
    fi

    if ! is_positive_integer "${detected:-}"; then
        log_warn "Unable to determine effective CPU count; using one job"
        detected=1
    fi
    detected=$((10#$detected))

    quota_count="$(cgroup_cpu_count)"
    if is_positive_integer "${quota_count:-}" && ((quota_count < detected)); then
        detected="$quota_count"
    fi

    CPU_COUNT="$detected"
}

detect_memory() {
    local total_kb=""
    local available_kb=""
    local cgroup_available_mb=""

    total_kb="$(awk '$1 == "MemTotal:" {print $2; exit}' /proc/meminfo 2>/dev/null || true)"
    available_kb="$(awk '$1 == "MemAvailable:" {print $2; exit}' /proc/meminfo 2>/dev/null || true)"

    if [[ "$total_kb" =~ ^[0-9]+$ ]]; then
        MEM_TOTAL_MB=$((total_kb / 1024))
    fi
    if [[ "$available_kb" =~ ^[0-9]+$ ]]; then
        MEM_AVAILABLE_MB=$((available_kb / 1024))
        MEMORY_KNOWN=true
    fi

    cgroup_available_mb="$(cgroup_memory_available_mb)"
    if [[ "$cgroup_available_mb" =~ ^[0-9]+$ ]]; then
        if [[ "$MEMORY_KNOWN" == false ]] || ((cgroup_available_mb < MEM_AVAILABLE_MB)); then
            MEM_AVAILABLE_MB="$cgroup_available_mb"
            MEMORY_KNOWN=true
        fi
    fi

    if [[ "$MEMORY_KNOWN" == false ]]; then
        log_warn "Unable to determine available memory; automatic jobs will use CPU capacity only"
    fi
}

calculate_recommended_jobs() {
    CPU_JOB_LIMIT=$((CPU_COUNT * 3 / 4))
    ((CPU_JOB_LIMIT > 0)) || CPU_JOB_LIMIT=1
    RECOMMENDED_JOBS="$CPU_JOB_LIMIT"

    if [[ "$MEMORY_KNOWN" == true ]]; then
        MEMORY_JOB_LIMIT=$((MEM_AVAILABLE_MB / MEMORY_PER_JOB_MB))
        if ((MEMORY_JOB_LIMIT < RECOMMENDED_JOBS)); then
            RECOMMENDED_JOBS="$MEMORY_JOB_LIMIT"
        fi
    fi
}

select_jobs() {
    calculate_recommended_jobs
    if [[ -z "$JOBS" ]]; then
        if ((RECOMMENDED_JOBS < 1)); then
            die "Only ${MEM_AVAILABLE_MB}MB is available; automatic builds require at least ${MEMORY_PER_JOB_MB}MB. Free memory or explicitly override with -j 1"
        fi
        JOBS="$RECOMMENDED_JOBS"
        JOBS_SOURCE="automatic"
    elif ((RECOMMENDED_JOBS < 1)); then
        log_warn "Only ${MEM_AVAILABLE_MB}MB is available, below the ${MEMORY_PER_JOB_MB}MB automatic per-job allowance; explicit -j$JOBS will be attempted"
    elif ((JOBS > RECOMMENDED_JOBS)); then
        log_warn "Requested -j$JOBS via $JOBS_SOURCE exceeds the safe automatic recommendation (-j$RECOMMENDED_JOBS)"
    fi
}

check_toolchain() {
    if ! command -v cargo >/dev/null 2>&1; then
        if [[ -x "$HOME/.cargo/bin/cargo" ]]; then
            export PATH="$HOME/.cargo/bin:$PATH"
        else
            die "cargo was not found in PATH"
        fi
    fi
    command -v rustc >/dev/null 2>&1 || die "rustc was not found in PATH"

    log_ok "$(cargo --version)"
    log_ok "$(rustc --version)"
}

check_repo() {
    [[ -f "$IC_DIR/Cargo.toml" ]] || \
        die "Cargo.toml was not found at $IC_DIR/Cargo.toml"
    [[ -f "$IC_DIR/Cargo.lock" ]] || \
        die "Cargo.lock was not found at $IC_DIR/Cargo.lock; locked builds require it"
    log_ok "Repo: $REPO_ROOT"
}

prepare_target_dir() {
    local fs_type=""
    local available_kb=""
    local available_mb=0
    local canonical_target=""

    if [[ -e "$TARGET_DIR" && ! -d "$TARGET_DIR" ]]; then
        die "Target path exists but is not a directory: $TARGET_DIR"
    fi
    mkdir -p -- "$TARGET_DIR" || die "Unable to create target directory: $TARGET_DIR"
    canonical_target="$(realpath -e -- "$TARGET_DIR")" || \
        die "Unable to resolve created target directory: $TARGET_DIR"
    [[ "$canonical_target" != "/" ]] || die "The filesystem root cannot be used as CARGO_TARGET_DIR" 2
    TARGET_DIR="$canonical_target"

    if command -v df >/dev/null 2>&1; then
        fs_type="$(df -PT "$TARGET_DIR" 2>/dev/null | awk 'NR == 2 {print $2}' || true)"
        case "$fs_type" in
            tmpfs|ramfs)
                log_warn "Target directory uses $fs_type and may run out of memory during a full build"
                ;;
        esac

        available_kb="$(df -Pk "$TARGET_DIR" 2>/dev/null | awk 'NR == 2 {print $4}' || true)"
        if [[ "$available_kb" =~ ^[0-9]+$ ]]; then
            available_mb=$((available_kb / 1024))
            log_ok "Target: $TARGET_DIR (${available_mb}MB disk available${fs_type:+, $fs_type})"
            if ((available_mb < 10240)); then
                log_warn "Less than 10GB is available in the target filesystem; a full build may exhaust it"
            fi
            return
        fi
    fi

    log_ok "Target: $TARGET_DIR"
}

prepare_log_dir() {
    if [[ -e "$BUILD_LOG_DIR" && ! -d "$BUILD_LOG_DIR" ]]; then
        die "Build log path exists but is not a directory: $BUILD_LOG_DIR"
    fi
    mkdir -p -- "$BUILD_LOG_DIR" || die "Unable to create build log directory: $BUILD_LOG_DIR"
}

acquire_build_lock() {
    [[ "$DRY_RUN" == false ]] || return 0

    if ! command -v flock >/dev/null 2>&1; then
        if [[ "$DO_CLEAN" == true ]]; then
            die "flock is required for a concurrency-safe --clean build"
        fi
        log_warn "flock is unavailable; relying on Cargo's internal build locking"
        return 0
    fi

    BUILD_LOCK_FILE="${TARGET_DIR%/}.lunarwing-build.lock"
    if [[ -L "$BUILD_LOCK_FILE" || ( -e "$BUILD_LOCK_FILE" && ! -f "$BUILD_LOCK_FILE" ) ]]; then
        die "Build lock path is not a regular file: $BUILD_LOCK_FILE"
    fi
    exec {BUILD_LOCK_FD}>> "$BUILD_LOCK_FILE" || die "Unable to open build lock: $BUILD_LOCK_FILE"
    if ! flock -n "$BUILD_LOCK_FD"; then
        die "Another build-lunarwing process is using target directory: $TARGET_DIR"
    fi
    log_ok "Build lock: $BUILD_LOCK_FILE"
}

configure_rustc_wrapper() {
    if [[ -n "${RUSTC_WRAPPER:-}" ]]; then
        log_ok "Using configured RUSTC_WRAPPER: $RUSTC_WRAPPER"
    elif command -v sccache >/dev/null 2>&1; then
        export RUSTC_WRAPPER="sccache"
        log_ok "sccache enabled"
    fi
}

format_command() {
    printf '%q ' "$@"
    printf '\n'
}

run_clean() {
    if [[ "$DO_CLEAN" != true ]]; then
        return 0
    fi

    if [[ "$DRY_RUN" == true ]]; then
        log_info "Would run: $(format_command cargo clean)"
        return
    fi

    log_info "Running cargo clean"
    (cd "$IC_DIR" && cargo clean)
    log_ok "Clean complete"
}

run_build() {
    local -a cargo_cmd=(cargo build --locked --bin lunarwing -j "$JOBS")
    local profile_dir="$PROFILE"
    local log_file=""
    local start_time=0
    local end_time=0
    local elapsed=0
    local exit_code=0
    local crate_count=0
    local target_size="unknown"
    local binary=""
    local binary_size=""

    if [[ "$PROFILE" == "release" ]]; then
        cargo_cmd+=(--release)
    fi

    log_info "Command: $(format_command "${cargo_cmd[@]}")"
    if [[ "$DRY_RUN" == true ]]; then
        log_ok "Dry run complete; Cargo build was not invoked"
        return
    fi

    log_file="$(mktemp "$BUILD_LOG_DIR/lunarwing-build.XXXXXX.log")" || \
        die "Unable to create a build log in $BUILD_LOG_DIR"
    log_info "Build log: $log_file"
    start_time="$(date +%s)"

    set +e
    if [[ "$VERBOSE" == true ]]; then
        (
            set -o pipefail
            cd "$IC_DIR" || exit 1
            "${cargo_cmd[@]}" 2>&1 | tee "$log_file"
        )
        exit_code=$?
    else
        (cd "$IC_DIR" && "${cargo_cmd[@]}" > "$log_file" 2>&1)
        exit_code=$?
    fi
    set -e

    end_time="$(date +%s)"
    elapsed=$((end_time - start_time))

    if ((exit_code != 0)); then
        log_error "Build failed with exit code $exit_code"
        log_error "Build log: $log_file"
        printf '\nLast 30 lines of build output:\n' >&2
        tail -n 30 "$log_file" >&2 || true
        exit "$exit_code"
    fi

    crate_count="$(grep -c '^ *Compiling ' "$log_file" 2>/dev/null || true)"
    target_size="$(du -sh "$TARGET_DIR" 2>/dev/null | awk '{print $1}' || true)"
    [[ -n "$target_size" ]] || target_size="unknown"
    binary="$(find "$TARGET_DIR" -maxdepth 3 -type f -path "*/$profile_dir/lunarwing" -perm -u+x -print -quit 2>/dev/null || true)"

    log_ok "Build succeeded in $((elapsed / 60))m $((elapsed % 60))s"
    log_ok "Crates compiled: $crate_count"
    log_ok "Target size: $target_size"
    log_ok "Build log: $log_file"

    if [[ -x "$binary" ]]; then
        binary_size="$(du -h "$binary" 2>/dev/null | awk '{print $1}' || true)"
        log_ok "Binary: $binary${binary_size:+ ($binary_size)}"
    else
        log_warn "Cargo succeeded but no executable LunarWing binary was found under $TARGET_DIR"
    fi
}

run_wasm_notice() {
    if [[ "$DO_WASM" == true ]]; then
        log_warn "--wasm is a compatibility no-op; build supported WASM extensions through their dedicated build paths"
    fi
}

print_summary() {
    log_info "Platform: $OS $ARCH"
    log_info "Effective CPUs: $CPU_COUNT; 75% CPU limit: $CPU_JOB_LIMIT job(s)"
    if [[ "$MEMORY_KNOWN" == true ]]; then
        log_info "Memory: ${MEM_TOTAL_MB}MB total, ${MEM_AVAILABLE_MB}MB available; memory limit: $MEMORY_JOB_LIMIT job(s)"
    fi
    log_info "Selected jobs: $JOBS ($JOBS_SOURCE)"
    log_info "Profile: $PROFILE"
}

main() {
    parse_args "$@"
    validate_config
    detect_platform
    detect_cgroup_paths
    detect_cpu_count
    detect_memory
    select_jobs

    check_toolchain
    check_repo
    prepare_target_dir
    export CARGO_TARGET_DIR="$TARGET_DIR"
    prepare_log_dir
    acquire_build_lock
    configure_rustc_wrapper
    print_summary
    run_clean
    run_build
    run_wasm_notice
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
