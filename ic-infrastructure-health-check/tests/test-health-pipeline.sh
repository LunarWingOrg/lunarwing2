#!/usr/bin/env bash
# Regression suite for health-pipeline.sh (WI#232 G2): the deployed-pipeline
# version/drift check. Runs the REAL check against sandbox copies of the
# pipeline scripts, driving every classification branch:
#   healthy / hand-edited / file-deleted / stale-expected / pre-WI#232 deploy
#   (no stamp) / expected-unset / disabled / hashing-unavailable.
#
# Run:  bash tests/test-health-pipeline.sh   (exit 0 = all pass)

set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"

HP="$LIB_DIR/../health-pipeline.sh"
[[ -x "$HP" ]] || { echo "FATAL: health-pipeline.sh not found/executable at $HP"; exit 1; }

# Sandbox copy of the deployed pipeline (top-level *.sh only, like cp -a of the
# installer — tests/ and init-templates/ are not part of the hashed surface).
mk_deploy() {
    local d
    d="$(sb)"
    mkdir -p "$d/deploy"
    cp "$LIB_DIR"/../*.sh "$d/deploy/"
    chmod +x "$d/deploy/"*.sh
    printf '%s' "$d/deploy"
}

# stamp_of <deploy-dir> — what ensure_health_pipeline would have written for
# that content. Sources the REAL mt-admin composition root (the health-pipeline
# module's hash function) so the test tracks the implementation, not a copy.
MT_ADMIN="$DIR/../../ic/scripts/lunarwing-mt-admin.sh"
stamp_of() {
    [[ -r "$MT_ADMIN" ]] || { echo "FATAL: mt-admin not found at $MT_ADMIN" >&2; exit 1; }
    env LUNARWING_SERVICE_MANAGER=systemd bash -c '
        source "$1"
        HEALTH_SRC_DIR="$2"
        health_pipeline_tree_hash "$HEALTH_SRC_DIR"
    ' _ "$MT_ADMIN" "$1"
}

# run_hp <deploy-dir> [expected-version] [extra env assignments...]
# The expected version is passed POSITIONALLY (not as a caller env prefix): an
# inline VAR=... prefix on a shell-function call scopes the variable to the
# function, and the explicit `env -u` below would strip it again.
run_hp() {
    local deploy="$1" expected="${2:-}"
    shift
    [[ $# -gt 0 ]] && shift
    env -u HEALTH_PIPELINE_EXPECTED_VERSION \
        ${expected:+HEALTH_PIPELINE_EXPECTED_VERSION="$expected"} "$@" \
        "$deploy/health-pipeline.sh" 2>/dev/null
}

echo "=== Section V: fresh deploy ==="

# V1 — freshly stamped deploy + matching expected version → healthy, exit 0.
d="$(mk_deploy)"
stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
out="$(run_hp "$d" "$stamp")"; RC=$?
assert_eq "$RC" "0" "V1: fresh deploy exits 0"
assert_eq "$(jq -r .status <<<"$out")" "healthy" "V1: fresh deploy is healthy"
assert_eq "$(jq -r '.metrics.pipeline_version' <<<"$out")" "$stamp" "V1: pipeline_version reported"

# V2 — pipeline_version equals the live hash for a fresh deploy.
assert_eq "$(jq -r '.metrics.live_version' <<<"$out")" "$stamp" "V2: live_version matches stamp"

echo "=== Section D: drift ==="

# D1 — hand-edited deploy file → degraded, exit 1, visible issue.
d="$(mk_deploy)"; stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
echo "# hand edit" >> "$d/health-xmpp.sh"
out="$(run_hp "$d" "$stamp")"; RC=$?
assert_eq "$RC" "1" "D1: hand-edited deploy exits 1"
assert_eq "$(jq -r .status <<<"$out")" "degraded" "D1: hand-edited deploy is degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$out")" "modified after install" "D1: hand-edit issue is visible"

# D2 — deleted deploy file → degraded (deletion, not just edits, changes the hash).
d="$(mk_deploy)"; stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
rm "$d/health-models.sh"
out="$(run_hp "$d" "$stamp")"; RC=$?
assert_eq "$RC" "1" "D2: deleted file exits 1"
assert_eq "$(jq -r .status <<<"$out")" "degraded" "D2: deleted file is degraded"

# D3 — stale deploy (live != expected, stamp intact): the skipped-refresh case.
d="$(mk_deploy)"; stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
out="$(run_hp "$d" stale-tree-hash)"; RC=$?
assert_eq "$RC" "1" "D3: stale deploy exits 1"
assert_contains "$(jq -r '.issues[0]' <<<"$out")" "stale" "D3: stale-deploy issue names the drift"

# D4 — pre-WI#232 deploy (no stamp file): degraded with the upgrade remedy hint.
d="$(mk_deploy)"
out="$(run_hp "$d" anything)"; RC=$?
assert_eq "$RC" "1" "D4: stampless deploy exits 1"
assert_contains "$(jq -r '.issues[0]' <<<"$out")" "no version stamp" "D4: pre-WI#232 deploy flagged"

# D5 — expected version unset in the env file → degraded, provenance unrecorded.
d="$(mk_deploy)"; stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
out="$(run_hp "$d")"; RC=$?
assert_eq "$RC" "1" "D5: unset expected version exits 1"
assert_contains "$(jq -r '.issues[0]' <<<"$out")" "HEALTH_PIPELINE_EXPECTED_VERSION unset" "D5: unrecorded provenance flagged"

echo "=== Section C: config ==="

# C1 — HEALTH_PIPELINE_CHECK_ENABLED=false → healthy/disabled, exit 0.
d="$(mk_deploy)"
out="$(run_hp "$d" "" HEALTH_PIPELINE_CHECK_ENABLED=false)"; RC=$?
assert_eq "$RC" "0" "C1: disabled check exits 0"
assert_eq "$(jq -r .status <<<"$out")" "healthy" "C1: disabled check reports healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$out")" "false" "C1: metrics record enabled=false"

# C2 — no sha256 tool anywhere → degraded (cannot verify), exit 1.
d="$(mk_deploy)"
mkdir -p "$d/nobin"
for tool in sha256sum shasum openssl find; do
    printf '#!/usr/bin/env bash\nexit 1\n' > "$d/nobin/$tool"
    chmod +x "$d/nobin/$tool"
done
out="$(run_hp "$d" "" PATH="$d/nobin:/usr/bin:/bin")"; RC=$?
assert_eq "$RC" "1" "C2: hashless environment exits 1"
assert_eq "$(jq -r .status <<<"$out")" "degraded" "C2: hashless environment is degraded"

echo "=== Section J: JSON contract ==="

# J1 — every branch emits the component name and a timestamp; issues is always
# an array; degraded branches carry pipeline_version/live_version/expected.
d="$(mk_deploy)"; stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
for env_val in "$stamp" "stale"; do
    out="$(run_hp "$d" "$env_val")"
    assert_eq "$(jq -r .component <<<"$out")" "pipeline" "J1: component is 'pipeline' (expected=$env_val)"
    assert_eq "$(jq -r '.issues | type' <<<"$out")" "array" "J1: issues is an array (expected=$env_val)"
    assert_eq "$(jq -r '.timestamp | type' <<<"$out")" "string" "J1: timestamp present (expected=$env_val)"
done

# J2 — the driver contract: status strings are only healthy|degraded (never
# critical), so self-heal can map the component but never page per-tenant.
d="$(mk_deploy)"; stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
echo "# drift" >> "$d/health-gateway.sh"
out="$(HEALTH_PIPELINE_EXPECTED_VERSION="$stamp" run_hp "$d")"
assert_eq "$(jq -r .status <<<"$out")" "degraded" "J2: drift is degraded (never critical)"

echo "=== Section S: stamp stability ==="

# S1 — identical content in two directories stamps identically (the deploy vs
# source-tree comparability contract: hashing is path-independent).
d1="$(mk_deploy)"; d2="$(mk_deploy)"
s1="$(stamp_of "$d1")"; s2="$(stamp_of "$d2")"
assert_eq "$s1" "$s2" "S1: identical content → identical stamp (path-independent)"

# S2 — the stamp equals what a fresh live hash of the same deploy computes.
d="$(mk_deploy)"; stamp="$(stamp_of "$d")"
printf '%s\n' "$stamp" > "$d/.pipeline-version"
out="$(HEALTH_PIPELINE_EXPECTED_VERSION="$stamp" run_hp "$d")"
assert_eq "$(jq -r '.metrics.live_version' <<<"$out")" "$stamp" "S2: stamp == live hash on a fresh deploy"

finish
