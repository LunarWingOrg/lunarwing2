#!/usr/bin/env bash
set -euo pipefail

# CI script: check that version bumps accompany WIT or extension source changes.
# Exit 0 if all checks pass, exit 1 if any version wasn't bumped.

ERRORS=0

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "${SCRIPT_DIR}/.." && pwd)
REPO_ROOT=$(git -C "$PROJECT_ROOT" rev-parse --show-toplevel)
PROJECT_PREFIX=$(git -C "$PROJECT_ROOT" rev-parse --show-prefix)
PROJECT_PREFIX=${PROJECT_PREFIX%/}

cd "$PROJECT_ROOT"

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq is required for registry version checks." >&2
    exit 2
fi

repo_file_path() {
    local file="$1"
    if [[ -n "$PROJECT_PREFIX" ]]; then
        printf '%s/%s' "$PROJECT_PREFIX" "$file"
    else
        printf '%s' "$file"
    fi
}

# --- Determine base branch and changed files ----------------------------------

BASE_BRANCH="${BASE_BRANCH:-}"
BASE_SHA="${BASE_SHA:-}"
CHANGE_LABELS="${CHANGE_LABELS:-}"

if [[ -n "$BASE_SHA" ]]; then
    if ! git -C "$REPO_ROOT" rev-parse --verify "$BASE_SHA^{commit}" >/dev/null 2>&1; then
        echo "Fetching base commit $BASE_SHA..."
        git -C "$REPO_ROOT" fetch origin "$BASE_SHA" --depth=1
    fi
    BASE_REF="$BASE_SHA"
    echo "Base commit: $BASE_REF"
else
    if [[ -z "$BASE_BRANCH" ]]; then
        remote_head=$(git -C "$REPO_ROOT" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)
        BASE_BRANCH=${remote_head#origin/}
    fi
    if [[ -z "$BASE_BRANCH" ]]; then
        if git -C "$REPO_ROOT" rev-parse --verify origin/master >/dev/null 2>&1; then
            BASE_BRANCH=master
        elif git -C "$REPO_ROOT" rev-parse --verify origin/main >/dev/null 2>&1; then
            BASE_BRANCH=main
        else
            echo "ERROR: set BASE_BRANCH or BASE_SHA to a valid comparison base." >&2
            exit 2
        fi
    fi

    BASE_REF="origin/${BASE_BRANCH}"
    echo "Base branch: $BASE_BRANCH"
    if ! git -C "$REPO_ROOT" rev-parse --verify "$BASE_REF^{commit}" >/dev/null 2>&1; then
        echo "Fetching $BASE_REF..."
        git -C "$REPO_ROOT" fetch origin "$BASE_BRANCH:refs/remotes/origin/$BASE_BRANCH" --depth=1
    fi
fi

# --- Skip mechanism -----------------------------------------------------------

if [[ ",${CHANGE_LABELS}," == *",skip-version-check,"* ]]; then
    if [[ "${ALLOW_VERSION_CHECK_SKIP:-0}" != "1" ]]; then
        echo "ERROR: skip-version-check requires maintainer-controlled ALLOW_VERSION_CHECK_SKIP=1." >&2
        exit 1
    fi
    echo "Authorized skip-version-check label detected - skipping all version checks."
    exit 0
fi

if [[ -n "$PROJECT_PREFIX" ]]; then
    CHANGED_FILES=$(git -C "$REPO_ROOT" diff --name-only --relative="$PROJECT_PREFIX" "$BASE_REF...HEAD" -- "$PROJECT_PREFIX")
else
    CHANGED_FILES=$(git -C "$REPO_ROOT" diff --name-only "$BASE_REF...HEAD")
fi

if [[ -z "$CHANGED_FILES" ]]; then
    echo "No changed files detected. Nothing to check."
    exit 0
fi

# --- Helper functions ---------------------------------------------------------

# Extract the version from a WIT package line like: package lunarwing:agent@1.2.3;
extract_wit_version() {
    local file="$1"
    if [[ ! -f "$file" ]]; then
        echo ""
        return
    fi
    sed -n 's/^[[:space:]]*package[[:space:]]\+[^@]*@\([0-9][0-9.]*[0-9]\)[[:space:]]*;.*/\1/p' "$file" \
        | head -n1
}

# Extract version from the base branch copy of a file
extract_wit_version_base() {
    local file="$1"
    git show "$BASE_REF:$(repo_file_path "$file")" 2>/dev/null \
        | sed -n 's/^[[:space:]]*package[[:space:]]\+[^@]*@\([0-9][0-9.]*[0-9]\)[[:space:]]*;.*/\1/p' \
        | head -n1 || true
}

# Extract a Rust string constant value: pub const NAME: &str = "value";
extract_rust_const() {
    local file="$1"
    local const_name="$2"
    if [[ ! -f "$file" ]]; then
        echo ""
        return
    fi
    sed -n "s/^.*${const_name}[[:space:]]*:[[:space:]]*&str[[:space:]]*=[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$file" \
        | head -n1
}

# Extract JSON "version" field using jq
extract_json_version() {
    local file="$1"
    if [[ ! -f "$file" ]]; then
        echo ""
        return
    fi
    jq -r '.version // empty' "$file" 2>/dev/null || true
}

# Extract JSON "version" from the base branch copy of a file
extract_json_version_base() {
    local file="$1"
    git show "$BASE_REF:$(repo_file_path "$file")" 2>/dev/null | jq -r '.version // empty' 2>/dev/null || true
}

# Return 0 if $1 (new) is strictly greater than $2 (old) via sort -V, or old is empty.
version_was_bumped() {
    local new="$1"
    local old="$2"
    if [[ -z "$old" ]]; then
        # No prior version — treat as new, no bump required
        return 0
    fi
    if [[ -z "$new" ]]; then
        # Version was removed — that's a problem
        return 1
    fi
    if [[ "$new" == "$old" ]]; then
        return 1
    fi
    # Check new > old via sort -V
    local highest
    highest=$(printf '%s\n%s\n' "$new" "$old" | sort -V | tail -n1)
    [[ "$highest" == "$new" ]]
}

# --- 1. WIT changes ----------------------------------------------------------

WIT_TOOL_CHANGED=false
WIT_CHANNEL_CHANGED=false

if echo "$CHANGED_FILES" | grep -qx 'wit/tool\.wit'; then
    WIT_TOOL_CHANGED=true
fi
if echo "$CHANGED_FILES" | grep -qx 'wit/channel\.wit'; then
    WIT_CHANNEL_CHANGED=true
fi

if $WIT_TOOL_CHANGED; then
    echo ""
    echo "=== wit/tool.wit changed ==="

    NEW_VER=$(extract_wit_version "wit/tool.wit")
    OLD_VER=$(extract_wit_version_base "wit/tool.wit")
    echo "  WIT package version: ${OLD_VER:-<none>} -> ${NEW_VER:-<missing>}"

    if ! version_was_bumped "${NEW_VER}" "${OLD_VER}"; then
        echo "  ERROR: wit/tool.wit package version was not bumped (${OLD_VER} -> ${NEW_VER:-<missing>})."
        ERRORS=$((ERRORS + 1))
    else
        echo "  OK: WIT package version bumped."
    fi

    # Check WIT_TOOL_VERSION constant matches
    CONST_VER=$(extract_rust_const "src/tools/wasm/mod.rs" "WIT_TOOL_VERSION")
    if [[ -n "$NEW_VER" && "$CONST_VER" != "$NEW_VER" ]]; then
        echo "  ERROR: WIT_TOOL_VERSION in src/tools/wasm/mod.rs is '${CONST_VER}' but wit/tool.wit has '${NEW_VER}'. They must match."
        ERRORS=$((ERRORS + 1))
    elif [[ -n "$NEW_VER" ]]; then
        echo "  OK: WIT_TOOL_VERSION matches wit/tool.wit."
    fi
fi

if $WIT_CHANNEL_CHANGED; then
    echo ""
    echo "=== wit/channel.wit changed ==="

    NEW_VER=$(extract_wit_version "wit/channel.wit")
    OLD_VER=$(extract_wit_version_base "wit/channel.wit")
    echo "  WIT package version: ${OLD_VER:-<none>} -> ${NEW_VER:-<missing>}"

    if ! version_was_bumped "${NEW_VER}" "${OLD_VER}"; then
        echo "  ERROR: wit/channel.wit package version was not bumped (${OLD_VER} -> ${NEW_VER:-<missing>})."
        ERRORS=$((ERRORS + 1))
    else
        echo "  OK: WIT package version bumped."
    fi

    # Check WIT_CHANNEL_VERSION constant matches
    CONST_VER=$(extract_rust_const "src/tools/wasm/mod.rs" "WIT_CHANNEL_VERSION")
    if [[ -n "$NEW_VER" && "$CONST_VER" != "$NEW_VER" ]]; then
        echo "  ERROR: WIT_CHANNEL_VERSION in src/tools/wasm/mod.rs is '${CONST_VER}' but wit/channel.wit has '${NEW_VER}'. They must match."
        ERRORS=$((ERRORS + 1))
    elif [[ -n "$NEW_VER" ]]; then
        echo "  OK: WIT_CHANNEL_VERSION matches wit/channel.wit."
    fi
fi

if $WIT_TOOL_CHANGED || $WIT_CHANNEL_CHANGED; then
    echo ""
    echo "  WARNING: WIT interface changed. All published registry extensions should bump their versions for compatibility."
fi

# --- 2. Tool source changes ---------------------------------------------------

TOOL_NAMES=$(echo "$CHANGED_FILES" | sed -n 's|^tools-src/\([^/]*\)/.*|\1|p' | sort -u)

if [[ -n "$TOOL_NAMES" ]]; then
    echo ""
    echo "=== Tool source changes ==="
fi

for tool in $TOOL_NAMES; do
    REGISTRY_FILE="registry/tools/${tool}.json"
    echo ""
    echo "  --- tools-src/${tool}/ changed ---"

    if [[ ! -f "$REGISTRY_FILE" ]]; then
        echo "  ERROR: ${REGISTRY_FILE} is required for tools-src/${tool}/."
        ERRORS=$((ERRORS + 1))
        continue
    fi

    NEW_VER=$(extract_json_version "$REGISTRY_FILE")
    OLD_VER=$(extract_json_version_base "$REGISTRY_FILE")

    echo "  Registry version: ${OLD_VER:-<none>} -> ${NEW_VER:-<missing>}"

    if ! version_was_bumped "${NEW_VER}" "${OLD_VER}"; then
        echo "  ERROR: ${REGISTRY_FILE} version was not bumped (${OLD_VER} -> ${NEW_VER:-<missing>}). Bump the version when changing tools-src/${tool}/."
        ERRORS=$((ERRORS + 1))
    else
        echo "  OK: version bumped."
    fi
done

# --- 3. Channel source changes ------------------------------------------------

CHANNEL_NAMES=$(echo "$CHANGED_FILES" | sed -n 's|^channels-src/\([^/]*\)/.*|\1|p' | sort -u)

if [[ -n "$CHANNEL_NAMES" ]]; then
    echo ""
    echo "=== Channel source changes ==="
fi

for channel in $CHANNEL_NAMES; do
    REGISTRY_FILE="registry/channels/${channel}.json"
    echo ""
    echo "  --- channels-src/${channel}/ changed ---"

    if [[ ! -f "$REGISTRY_FILE" ]]; then
        echo "  ERROR: ${REGISTRY_FILE} is required for channels-src/${channel}/."
        ERRORS=$((ERRORS + 1))
        continue
    fi

    NEW_VER=$(extract_json_version "$REGISTRY_FILE")
    OLD_VER=$(extract_json_version_base "$REGISTRY_FILE")

    echo "  Registry version: ${OLD_VER:-<none>} -> ${NEW_VER:-<missing>}"

    if ! version_was_bumped "${NEW_VER}" "${OLD_VER}"; then
        echo "  ERROR: ${REGISTRY_FILE} version was not bumped (${OLD_VER} -> ${NEW_VER:-<missing>}). Bump the version when changing channels-src/${channel}/."
        ERRORS=$((ERRORS + 1))
    else
        echo "  OK: version bumped."
    fi
done

# --- Summary ------------------------------------------------------------------

echo ""
if [[ $ERRORS -gt 0 ]]; then
    echo "FAILED: ${ERRORS} version check(s) did not pass. See errors above."
    exit 1
else
    echo "All version checks passed."
    exit 0
fi
