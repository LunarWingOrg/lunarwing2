#!/usr/bin/env bash
# ============================================================
# build-lunarwing.sh — LunarWing native build script
# ============================================================
#
# Builds the LunarWing (ic) crate natively. Auto-detects
# architecture and optimizes defaults accordingly:
#
#   aarch64  → low jobs, strict memory checking (Pi 5, etc.)
#   x86_64   → speed optimized, assumes ≥8GB free RAM
#
# Usage:
#   ./scripts/build-lunarwing.sh [OPTIONS]
#
# Options:
#   -c, --clean       Run cargo clean before building
#   -j, --jobs N      Number of parallel jobs (auto-detected by default)
#   -t, --target DIR  Override CARGO_TARGET_DIR
#   -r, --repo DIR    Override repo root path
#   --profile MODE    Build profile: release (default) or debug
#   --wasm            Accepted for compatibility; WASM artifacts build elsewhere
#   --no-kill         Don't kill stale cargo/rustc processes
#   --no-locks        Don't clear stale lock files
#   -v, --verbose     Show cargo output in real-time
#   -h, --help        Show this help message
#
# Environment variables (override flags):
#   CARGO_TARGET_DIR  Build artifacts directory
#   LUNARWING_REPO   Repo root path
#   BUILD_JOBS        Number of parallel jobs
#
# Examples:
#   ./scripts/build-lunarwing.sh                    # Auto-detect arch, build
#   ./scripts/build-lunarwing.sh -c                 # Clean + build
#   ./scripts/build-lunarwing.sh -j 8               # Force 8 jobs
#   ./scripts/build-lunarwing.sh --profile debug    # Debug build
#   BUILD_JOBS=4 ./scripts/build-lunarwing.sh       # Env override

set -euo pipefail

# ── Defaults ───────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${LUNARWING_REPO:-$(cd "$SCRIPT_DIR/.." && pwd)}"
IC_DIR="$REPO_ROOT/ic"
TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target}"
WASM_TARGET_DIR="${TARGET_DIR}-wasm"
PROFILE="release"
DO_CLEAN=false
DO_WASM=false
DO_KILL=true
DO_LOCKS=true
VERBOSE=false

# ── Architecture detection ─────────────────────────────────

ARCH="$(uname -m)"
NPROC="$(nproc 2>/dev/null || echo 2)"

case "$ARCH" in
    aarch64|arm64)
        ARCH_FAMILY="arm"
        # ARM: conservative defaults. Pi 5 has 4-8GB RAM.
        # -j2 is the sweet spot; -j1 for ≤4GB systems.
        DEFAULT_JOBS=2
        MEM_WARN_MB=6144        # Warn if <6GB free
        MEM_MIN_JOBS2_MB=4096   # If <4GB free, force -j1
        MEM_HARD_MIN_MB=2048    # Refuse to build with <2GB free
        ;;
    x86_64|amd64)
        ARCH_FAMILY="x86"
        # x86: optimize for speed. Assume ≥8GB free RAM.
        # Use all cores — memory checks will throttle if RAM is actually low.
        DEFAULT_JOBS=$NPROC
        [ "$DEFAULT_JOBS" -lt 2 ] && DEFAULT_JOBS=2
        MEM_WARN_MB=8192        # Warn if <8GB free (below assumed baseline)
        MEM_MIN_JOBS2_MB=4096   # If <4GB free, throttle to -j2
        MEM_HARD_MIN_MB=4096    # Refuse to build with <4GB free
        ;;
    *)
        log_warn "Unknown architecture '$ARCH' — using conservative defaults"
        ARCH_FAMILY="unknown"
        DEFAULT_JOBS=1
        MEM_WARN_MB=4096
        MEM_MIN_JOBS2_MB=2048
        MEM_HARD_MIN_MB=2048
        ;;
esac

JOBS="${BUILD_JOBS:-$DEFAULT_JOBS}"

# ── Colors ─────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ── Helpers ────────────────────────────────────────────────

log_info()  { echo -e "${BLUE}[INFO]${NC}  $*"; }
log_ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*"; }

usage() {
    sed -n '2,/^# Examples:/p' "$0" | sed 's/^# *//' | sed 's/^#//'
    exit 0
}

check_cargo() {
    if ! command -v cargo &>/dev/null; then
        if [ -x "$HOME/.cargo/bin/cargo" ]; then
            export PATH="$HOME/.cargo/bin:$PATH"
        else
            log_error "cargo not found. Run install_rust_debian.sh first."
            exit 1
        fi
    fi
    log_ok "cargo $(cargo --version | awk '{print $2}')"
    log_ok "rustc $(rustc --version | awk '{print $2}')"
}

check_repo() {
    if [ ! -f "$IC_DIR/Cargo.toml" ]; then
        log_error "Cargo.toml not found at $IC_DIR/Cargo.toml"
        log_error "Is LUNARWING_REPO set correctly? (current: $REPO_ROOT)"
        exit 1
    fi
    log_ok "Repo: $REPO_ROOT"
}

check_target_dir() {
    # Warn if target dir is on tmpfs (will run out of space)
    local mount_point
    mount_point=$(df "$TARGET_DIR" 2>/dev/null | tail -1 | awk '{print $1}')
    if [[ "$mount_point" == *"tmpfs"* ]] || [[ "$mount_point" == *"tmp"* ]]; then
        log_warn "Target directory is on tmpfs — builds may fail due to space limits"
        log_warn "Consider: export CARGO_TARGET_DIR=/home/\$USER/.cargo-target"
    fi
    log_ok "Target: $TARGET_DIR"

    # ── Memory check (based on AVAILABLE memory, not total) ──
    local mem_total_kb
    mem_total_kb=$(awk '/MemTotal/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
    local mem_total_mb=$((mem_total_kb / 1024))
    local mem_avail_kb
    mem_avail_kb=$(awk '/MemAvailable/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
    local mem_avail_mb=$((mem_avail_kb / 1024))

    log_ok "Memory: ${mem_total_mb}MB total, ${mem_avail_mb}MB available"

    # Hard minimum — refuse to build (based on available RAM)
    if [ "$mem_avail_mb" -lt "$MEM_HARD_MIN_MB" ]; then
        log_error "Only ${mem_avail_mb}MB RAM available — minimum ${MEM_HARD_MIN_MB}MB required"
        log_error "Free up memory or use a machine with more RAM"
        exit 1
    fi

    # Architecture-specific job adjustment (based on available RAM)
    case "$ARCH_FAMILY" in
        arm)
            # ARM: strict memory enforcement
            if [ "$mem_avail_mb" -lt "$MEM_MIN_JOBS2_MB" ] && [ "$JOBS" -gt 1 ]; then
                log_warn "Only ${mem_avail_mb}MB RAM available — forcing -j1 to prevent OOM"
                JOBS=1
            elif [ "$mem_avail_mb" -lt "$MEM_WARN_MB" ]; then
                log_warn "Low available memory (${mem_avail_mb}MB) — OOM kills possible with -j${JOBS}"
                log_warn "Consider: -j 1  (or add swap)"
            fi
            ;;
        x86)
            # x86: assume ≥8GB free. Only intervene if genuinely low.
            if [ "$mem_avail_mb" -lt "$MEM_WARN_MB" ]; then
                log_warn "Only ${mem_avail_mb}MB free — expected ≥${MEM_WARN_MB}MB for optimal x86 builds"
            fi
            # Only throttle if we're below the hard floor for multi-job
            if [ "$mem_avail_mb" -lt "$MEM_MIN_JOBS2_MB" ] && [ "$JOBS" -gt 2 ]; then
                log_warn "Low available RAM (${mem_avail_mb}MB) — reducing jobs from $JOBS to 2"
                JOBS=2
            fi
            ;;
    esac
}

kill_stale_processes() {
    if [ "$DO_KILL" = false ]; then
        return
    fi

    local found=false
    if pgrep -f "cargo build" &>/dev/null || \
       pgrep -f "rustc" &>/dev/null; then
        found=true
    fi

    if [ "$found" = true ]; then
        log_info "Killing stale cargo/rustc processes..."
        pkill -9 -f "cargo build" 2>/dev/null || true
        pkill -9 -f "rustc" 2>/dev/null || true
        sleep 2

        # Verify they're dead
        if pgrep -f "cargo build" &>/dev/null; then
            log_warn "Some cargo processes survived — forcing kill"
            pkill -9 cargo 2>/dev/null || true
            pkill -9 rustc 2>/dev/null || true
            sleep 1
        fi
        log_ok "Stale processes killed"
    else
        log_ok "No stale cargo/rustc processes found"
    fi
}

clear_locks() {
    if [ "$DO_LOCKS" = false ]; then
        return
    fi

    local lock_count=0
    while IFS= read -r lockfile; do
        rm -f "$lockfile"
        lock_count=$((lock_count + 1))
    done < <(find "$TARGET_DIR" -type f \( -name ".cargo-lock" -o -name ".cargo-build-lock" -o -name ".cargo-artifact-lock" \) 2>/dev/null)

    while IFS= read -r lockfile; do
        rm -f "$lockfile"
        lock_count=$((lock_count + 1))
    done < <(find "$WASM_TARGET_DIR" -type f \( -name ".cargo-lock" -o -name ".cargo-build-lock" -o -name ".cargo-artifact-lock" \) 2>/dev/null)

    # Also clear stale locks in the global cargo home
    for f in "$HOME/.cargo/.package-cache" "$HOME/.cargo/.package-cache-mutate"; do
        if [ -f "$f" ]; then
            rm -f "$f"
            lock_count=$((lock_count + 1))
        fi
    done

    if [ "$lock_count" -gt 0 ]; then
        log_ok "Cleared $lock_count stale lock file(s)"
    else
        log_ok "No stale lock files found"
    fi
}

run_clean() {
    if [ "$DO_CLEAN" = true ]; then
        log_info "Running cargo clean..."
        (cd "$IC_DIR" && cargo clean) 2>&1
        log_ok "Clean complete"
    fi
}

run_build() {
    local profile_flag=""
    local profile_name="$PROFILE"
    if [ "$PROFILE" = "release" ]; then
        profile_flag="--release"
    fi

    # ── x86 speed optimizations ────────────────────────────
    if [ "$ARCH_FAMILY" = "x86" ]; then
        # Use sccache if available (faster incremental builds)
        if command -v sccache &>/dev/null; then
            export RUSTC_WRAPPER="sccache"
            log_ok "sccache enabled for incremental builds"
        else
            log_warn "sccache not found — incremental rebuilds will be slower"
            log_warn "Install:  cargo install sccache"
            log_warn "      or:  apt install sccache  /  brew install sccache"
            log_warn "Then re-run this script to auto-enable caching"
        fi
    fi

    local log_file="/tmp/cargo_build_$(date +%Y%m%d_%H%M%S).log"

    echo ""
    echo -e "${BOLD}${CYAN}╔══════════════════════════════════════════════════╗${NC}"
    echo -e "${BOLD}${CYAN}║         LunarWing Build — $profile_name profile"
    echo -e "${BOLD}${CYAN}╠══════════════════════════════════════════════════╣${NC}"
    echo -e "${BOLD}${CYAN}║${NC}  Arch:     ${BOLD}$ARCH ($ARCH_FAMILY)${NC}"
    echo -e "${BOLD}${CYAN}║${NC}  Jobs:     ${BOLD}$JOBS${NC}"
    echo -e "${BOLD}${CYAN}║${NC}  Target:   ${BOLD}$TARGET_DIR${NC}"
    echo -e "${BOLD}${CYAN}║${NC}  Repo:     ${BOLD}$REPO_ROOT${NC}"
    echo -e "${BOLD}${CYAN}║${NC}  Log:      ${BOLD}$log_file${NC}"
    echo -e "${BOLD}${CYAN}╚══════════════════════════════════════════════════╝${NC}"
    echo ""

    local start_time
    start_time=$(date +%s)

    local cargo_cmd="cargo build -j $JOBS $profile_flag"
    local exit_code=0

    if [ "$VERBOSE" = true ]; then
        log_info "Running: $cargo_cmd (verbose)"
        set +e
        (cd "$IC_DIR" && eval "$cargo_cmd" 2>&1 | tee "$log_file")
        exit_code=${PIPESTATUS[0]}
        set -e
    else
        log_info "Running: $cargo_cmd"
        log_info "Log: $log_file"
        set +e
        (cd "$IC_DIR" && eval "$cargo_cmd" > "$log_file" 2>&1)
        exit_code=$?
        set -e
    fi

    local end_time
    end_time=$(date +%s)
    local elapsed=$((end_time - start_time))
    local minutes=$((elapsed / 60))
    local seconds=$((elapsed % 60))

    echo ""
    if [ "$exit_code" -eq 0 ]; then
        local crate_count
        crate_count=$(grep -c "Compiling" "$log_file" 2>/dev/null || echo "?")
        local target_size
        target_size=$(du -sh "$TARGET_DIR" 2>/dev/null | awk '{print $1}')

        echo -e "${GREEN}${BOLD}✓ Build succeeded!${NC}"
        echo -e "  Arch:            ${BOLD}$ARCH${NC}"
        echo -e "  Crates compiled: ${BOLD}$crate_count${NC}"
        echo -e "  Time:            ${BOLD}${minutes}m ${seconds}s${NC}"
        echo -e "  Target size:     ${BOLD}$target_size${NC}"
        echo -e "  Log:             ${BOLD}$log_file${NC}"

        # Show binary location
        local binary
        binary=$(find "$TARGET_DIR/$PROFILE" -maxdepth 1 -type f -executable -name "lunarwing*" 2>/dev/null | head -1)
        if [ -n "$binary" ]; then
            local bin_size
            bin_size=$(du -h "$binary" 2>/dev/null | awk '{print $1}')
            echo -e "  Binary:          ${BOLD}$binary${NC} (${bin_size})"
        fi
    else
        echo -e "${RED}${BOLD}✗ Build failed!${NC}"
        echo -e "  Exit code: ${BOLD}$exit_code${NC}"
        echo -e "  Log:       ${BOLD}$log_file${NC}"
        echo ""
        echo -e "${YELLOW}Last 30 lines of build output:${NC}"
        tail -30 "$log_file" 2>/dev/null
        exit "$exit_code"
    fi
}

run_wasm_build() {
    if [ "$DO_WASM" = false ]; then
        return
    fi

    log_warn "--wasm no longer builds proprietary channel artifacts; build supported WASM extensions through mt-admin or their own build scripts"
}

# ── Parse arguments ────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case $1 in
        -c|--clean)    DO_CLEAN=true; shift ;;
        -j|--jobs)     JOBS="$2"; shift 2 ;;
        -t|--target)   TARGET_DIR="$2"; shift 2 ;;
        -r|--repo)     REPO_ROOT="$2"; IC_DIR="$REPO_ROOT/ic"; shift 2 ;;
        --profile)     PROFILE="$2"; shift 2 ;;
        --wasm)        DO_WASM=true; shift ;;
        --no-kill)     DO_KILL=false; shift ;;
        --no-locks)    DO_LOCKS=false; shift ;;
        -v|--verbose)  VERBOSE=true; shift ;;
        -h|--help)     usage ;;
        *)
            log_error "Unknown option: $1"
            usage
            ;;
    esac
done

# Export CARGO_TARGET_DIR so cargo actually uses it
export CARGO_TARGET_DIR="$TARGET_DIR"

# ── Main ───────────────────────────────────────────────────

echo ""
echo -e "${BOLD}🦅 LunarWing Build Script${NC}"
echo -e "   $(date '+%Y-%m-%d %H:%M:%S')"
echo -e "   Arch: ${BOLD}$ARCH${NC} ($ARCH_FAMILY profile)"
echo ""

check_cargo
check_repo
check_target_dir

echo ""
log_info "Phase 1: Cleanup"
kill_stale_processes
clear_locks

echo ""
log_info "Phase 2: Prepare"
run_clean

echo ""
log_info "Phase 3: Build"
run_build

echo ""
log_info "Phase 4: WASM (optional)"
run_wasm_build

echo ""
echo -e "${BOLD}${GREEN}🦅 All done.${NC}"
echo ""
