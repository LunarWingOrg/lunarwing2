#!/usr/bin/env bash
# Build all WASM tools and channels from source.
#
# Verifies that every tool/channel in the registry compiles against the
# current WIT definitions. Used by CI and can be run locally.
#
# Prerequisites:
#   rustup target add wasm32-wasip2
#
# Usage:
#   ./scripts/build-wasm-extensions.sh           # build all
#   ./scripts/build-wasm-extensions.sh --tools    # tools only
#   ./scripts/build-wasm-extensions.sh --channels # channels only
#   ./scripts/build-wasm-extensions.sh --strict   # fail-fast + validate manifests/capabilities
#
# --strict may be combined with --tools or --channels (order independent).

set -euo pipefail

cd "$(dirname "$0")/.."

# Ensure the wasm target is present on the *active* toolchain. Because we cd'd
# into the workspace above, rust-toolchain.toml selects the pinned build
# toolchain here, so the target lands on exactly the toolchain the cargo builds
# below use — not the caller's default toolchain, which is what caused
# "the wasm32-wasip2 target may not be installed" during tenant builds.
if ! rustup target list --installed 2>/dev/null | grep -q wasm32-wasip2; then
    echo "==> Installing wasm32-wasip2 target for the active toolchain..."
    rustup target add wasm32-wasip2
fi

BUILD_TOOLS=true
BUILD_CHANNELS=true
STRICT=false
FAILED=()

for arg in "${@:-}"; do
    case "$arg" in
        --tools) BUILD_CHANNELS=false ;;
        --channels) BUILD_TOOLS=false ;;
        --strict) STRICT=true ;;
    esac
done

build_extension() {
    local manifest_path="$1"
    local source_dir
    local crate_name
    local artifact
    local capabilities

    if $STRICT; then
        for field in '.source.dir' '.source.crate_name'; do
            if ! jq -e "$field" "$manifest_path" >/dev/null 2>&1; then
                echo "  FAIL $(basename "$manifest_path" .json) (manifest missing $field)"
                FAILED+=("$(basename "$manifest_path" .json)")
                return 1
            fi
        done
        source_dir=$(jq -r '.source.dir' "$manifest_path")
        capabilities=$(jq -r '.source.capabilities // empty' "$manifest_path")
        if [[ -n "$capabilities" && ! -f "$source_dir/$capabilities" ]]; then
            echo "  FAIL $(basename "$manifest_path" .json) (capabilities file missing: $capabilities)"
            FAILED+=("$(basename "$manifest_path" .json)")
            return 1
        fi
    fi

    source_dir=$(jq -r '.source.dir' "$manifest_path")
    crate_name=$(jq -r '.source.crate_name' "$manifest_path")
    artifact="$source_dir/target/wasm32-wasip2/release/${crate_name//-/_}.wasm"
    local name
    name=$(basename "$manifest_path" .json)

    if [ ! -d "$source_dir" ]; then
        echo "  FAIL $name (source dir $source_dir not found)"
        FAILED+=("$name")
        return 1
    fi

    echo "  BUILD $name ($crate_name) from $source_dir"
    rm -f "$artifact"
    if ! cargo build --locked --release --target wasm32-wasip2 --manifest-path "$source_dir/Cargo.toml" 2>&1; then
        echo "  FAIL $name"
        FAILED+=("$name")
        return 1
    fi
    if [[ ! -f "$artifact" ]]; then
        echo "  FAIL $name (expected artifact missing: $artifact)"
        FAILED+=("$name")
        return 1
    fi
    echo "  OK   $name"
}

if $BUILD_TOOLS; then
    echo "Building WASM tools..."
    for manifest in registry/tools/*.json; do
        if $STRICT; then
            build_extension "$manifest" || exit 1
        else
            build_extension "$manifest" || true
        fi
    done
fi

if $BUILD_CHANNELS; then
    echo "Building WASM channels..."
    for manifest in registry/channels/*.json; do
        if $STRICT; then
            build_extension "$manifest" || exit 1
        else
            build_extension "$manifest" || true
        fi
    done
fi

echo ""
if [ ${#FAILED[@]} -gt 0 ]; then
    echo "FAILED: ${FAILED[*]}"
    exit 1
else
    echo "All WASM extensions built successfully."
fi
