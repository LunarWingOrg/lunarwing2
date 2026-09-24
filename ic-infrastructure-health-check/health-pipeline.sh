#!/usr/bin/env bash
# Health Check: Deployed Pipeline Version (WI#232 G2)
#
# The host-global health/self-heal pipeline runs from a DEPLOYED copy at
# /usr/local/lib/lunarwing-health/, installed by mt-admin's
# ensure_health_pipeline (cp -a of the tree's ic-infrastructure-health-check/).
# WI#225/WI#228 established that a deploy left stale by a skipped refresh is a
# live hazard: stale self-heal code blindly restarted deliberately-stopped
# tenants' units and part of the "podman store corruption" signature was stale
# pipeline code doing root restarts. Nothing compared the deployed bytes against
# the tree, so drift stayed invisible until it misbehaved.
#
# This check closes that (WI#232 fix candidate G2): it hashes the deployed
# scripts it runs alongside and compares them against the version mt-admin
# recorded at the last install/refresh —
#   - HEALTH_PIPELINE_EXPECTED_VERSION in /etc/lunarwing/health.env, written
#     and reconciled by ensure_health_pipeline from the tree it deployed.
#   - .pipeline-version next to the deployed scripts, written post-sync by the
#     same path (records what the sync actually laid down).
#
# Status semantics (drift is a LATENT defect, not an outage — degraded, never
# critical; and the component name deliberately maps to no SERVICE_MAP service,
# so self-heal can never restart anything over it):
#   - live hash == expected                 -> healthy
#   - stamp absent (pre-WI#232 deploy)     -> degraded (unknown provenance)
#   - stamp/live mismatch                   -> degraded (deploy skipped refresh)
#   - live != expected (both known)         -> degraded (stale deploy)
#   - no hashing tool / unreadable env      -> degraded (cannot verify)
#
# Output: JSON to stdout; exit 0=healthy, 1=degraded, 2 never used here.
#
# Env vars:
#   HEALTH_PIPELINE_EXPECTED_VERSION — stamp of the tree the deploy came from
#                                     (written by mt-admin; empty = pre-WI#232)
#   HEALTH_PIPELINE_CHECK_ENABLED    — false disables the check entirely

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
EXPECTED="${HEALTH_PIPELINE_EXPECTED_VERSION:-}"

issues=()
metrics='{}'

# Hash the deployed pipeline's executable surface: sorted top-level *.sh, the
# same set and order ensure_health_pipeline stamps, so both sides are directly
# comparable. Files are hashed from INSIDE the dir so the digest stream carries
# bare basenames (hashing "$dir/$f" would embed the deploy path and never match
# a stamp computed against the source tree). Missing files are recorded in the
# stream so deletions (not just edits) change the hash.
pipeline_hash() {
    local dir="$1" digest
    [[ -d "$dir" ]] || return 1
    if command -v sha256sum >/dev/null 2>&1; then
        digest="$(find "$dir" -maxdepth 1 -type f -name '*.sh' -printf '%f\n' 2>/dev/null | LC_ALL=C sort \
            | ( cd "$dir" && while IFS= read -r f; do sha256sum "$f" 2>/dev/null || printf 'MISSING %s\n' "$f"; done ) \
            | sha256sum)"
    elif command -v shasum >/dev/null 2>&1; then
        digest="$(find "$dir" -maxdepth 1 -type f -name '*.sh' -printf '%f\n' 2>/dev/null | LC_ALL=C sort \
            | ( cd "$dir" && while IFS= read -r f; do shasum -a 256 "$f" 2>/dev/null || printf 'MISSING %s\n' "$f"; done ) \
            | shasum -a 256)"
    elif command -v openssl >/dev/null 2>&1; then
        digest="$(find "$dir" -maxdepth 1 -type f -name '*.sh' -printf '%f\n' 2>/dev/null | LC_ALL=C sort \
            | ( cd "$dir" && while IFS= read -r f; do openssl dgst -sha256 "$f" 2>/dev/null || printf 'MISSING %s\n' "$f"; done ) \
            | openssl dgst -sha256)"
    else
        return 1
    fi
    digest="${digest%% *}"
    [[ "$digest" =~ ^[[:xdigit:]]{16,}$ ]] || return 1
    printf '%s' "${digest,,}"
}

# Disabled by config: report healthy with enabled=false (monitoring choice, not
# a gap). Same contract as health-runtime.sh/health-knowledge.sh.
if [[ "${HEALTH_PIPELINE_CHECK_ENABLED:-true}" != "true" ]]; then
    printf '{"component":"pipeline","status":"healthy","timestamp":"%s","metrics":{"enabled":false},"issues":[]}\n' "$TIMESTAMP"
    exit 0
fi

live="$(pipeline_hash "$SCRIPT_DIR")"
hash_ok=$?
installed=""
if [[ -f "$SCRIPT_DIR/.pipeline-version" ]]; then
    installed="$(tr -d '[:space:]' < "$SCRIPT_DIR/.pipeline-version")"
fi

if [[ $hash_ok -ne 0 || -z "$live" ]]; then
    status="degraded"
    issues+=("deployed pipeline hash could not be computed (no sha256 tool or unreadable scripts)")
    metrics="$(jq -n '{enabled:true, hashed:false}')"
elif [[ -z "$installed" ]]; then
    # Pre-WI#232 deploy: provenance unknown. This is exactly the ajhermes
    # condition (Aug 31 health-systemd vs current tree) — loud, not silent.
    status="degraded"
    issues+=("deployed pipeline has no version stamp (predates WI#232); re-run upgrade-tenant or enable-health-fleet.sh to refresh and stamp it")
    metrics="$(jq -n --arg live "$live" '{enabled:true, pipeline_version:"unknown", live_version:$live}')"
elif [[ -z "$EXPECTED" ]]; then
    status="degraded"
    issues+=("HEALTH_PIPELINE_EXPECTED_VERSION unset; deploy provenance unrecorded — re-run ensure_health_pipeline (upgrade-tenant or enable-health-fleet.sh)")
    metrics="$(jq -n --arg ins "$installed" --arg live "$live" '{enabled:true, pipeline_version:$ins, live_version:$live, expected_version:"unset"}')"
elif [[ "$installed" != "$live" ]]; then
    status="degraded"
    issues+=("deployed pipeline files were modified after install (stamp $installed != live $live); redeploy or restore")
    metrics="$(jq -n --arg ins "$installed" --arg live "$live" --arg exp "$EXPECTED" '{enabled:true, pipeline_version:$ins, live_version:$live, expected_version:$exp}')"
elif [[ "$live" != "$EXPECTED" ]]; then
    status="degraded"
    issues+=("deployed pipeline is stale (live $live != tree $EXPECTED); run upgrade-tenant to refresh it")
    metrics="$(jq -n --arg ins "$installed" --arg live "$live" --arg exp "$EXPECTED" '{enabled:true, pipeline_version:$ins, live_version:$live, expected_version:$exp}')"
else
    status="healthy"
    metrics="$(jq -n --arg ins "$installed" --arg exp "$EXPECTED" '{enabled:true, pipeline_version:$ins, live_version:$ins, expected_version:$exp}')"
fi

issues_json="[]"
if [[ ${#issues[@]} -gt 0 ]]; then
    issues_json="$(printf '%s\n' "${issues[@]}" | jq -R . | jq -s .)"
fi

printf '{"component":"pipeline","status":"%s","timestamp":"%s","metrics":%s,"issues":%s}\n' \
    "$status" "$TIMESTAMP" "$metrics" "$issues_json"
[[ "$status" == "healthy" ]] && exit 0 || exit 1
