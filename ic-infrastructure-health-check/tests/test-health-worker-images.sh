#!/usr/bin/env bash
#
# Tests for health-worker-images.sh (WI#271): per-tenant image-store audit.
# Mocks podman/sudo/id/getent through a PATH shadow so no real store or tenant
# is touched. Store fixtures: $MOCK_STORE_ROOT/{admin,tenant}/<image-key>
# files hold the image ID; an absent file means the store lacks the image.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/../health-worker-images.sh"
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }
ROOT="$(mktemp -d "${TMPDIR:-/tmp}/worker-images-health-test.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/bin"
fail=0; pass=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s (%s)\n' "$1" "$2"; fail=$((fail + 1)); }
assert_eq() { [[ "$1" == "$2" ]] && ok "$3" || bad "$3" "got '$1', want '$2'"; }
assert_has() { grep -qF -- "$2" <<<"$1" && ok "$3" || bad "$3" "missing '$2'"; }

cat >"$ROOT/bin/podman" <<'MOCK'
#!/usr/bin/env bash
# image inspect -f {{.Id}} <image> — store keyed by MOCK_STORE (admin default).
if [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]]; then
  key="$(tr '/:' '__' <<<"${5}")"
  f="$MOCK_STORE_ROOT/${MOCK_STORE:-admin}/$key"
  [[ -f "$f" ]] || exit 1
  cat "$f"
  exit 0
fi
exit 0
MOCK

cat >"$ROOT/bin/sudo" <<'MOCK'
#!/usr/bin/env bash
# sudo -u <tenant> env KEY=VAL ... <cmd...> — strip the wrapper, mark the
# tenant store, and exec the (mocked) command.
shift 2
[[ "${1:-}" == env ]] && shift
while [[ "${1:-}" == *=* ]]; do shift; done
MOCK_STORE=tenant "$@"
MOCK

cat >"$ROOT/bin/id" <<'MOCK'
#!/usr/bin/env bash
[[ "${1:-}" == "-u" ]] || exit 1
printf '1001\n'
MOCK

cat >"$ROOT/bin/getent" <<'MOCK'
#!/usr/bin/env bash
[[ "${1:-}" == passwd ]] || exit 1
printf '%s:x:1001:1001::/home/%s:/bin/bash\n' "$2" "$2"
MOCK
chmod +x "$ROOT/bin/"*

export MOCK_STORE_ROOT="$ROOT/stores"
mkdir -p "$MOCK_STORE_ROOT/admin" "$MOCK_STORE_ROOT/tenant"

run_check() { PATH="$ROOT/bin:$PATH" bash "$CHECK" 2>/dev/null; }

set_images() { # <store> <vision-id|-> <voice-id|-> <pebble-id|->
  local store="$1"
  rm -rf "${MOCK_STORE_ROOT:?}/$store"; mkdir -p "$MOCK_STORE_ROOT/$store"
  [[ "$2" == - ]] || printf 'sha256:%s' "$2" >"$MOCK_STORE_ROOT/$store/lunarwing_vision-service_latest"
  [[ "$3" == - ]] || printf 'sha256:%s' "$3" >"$MOCK_STORE_ROOT/$store/lunarwing_voice-service_latest"
  [[ "$4" == - ]] || printf 'sha256:%s' "$4" >"$MOCK_STORE_ROOT/$store/lunarwing-worker-pebble_latest"
}

registry="$ROOT/ports.json"

# ── disabled by config ───────────────────────────────────────────────────────
out="$(HEALTH_WORKER_IMAGES_ENABLED=false run_check)"; rc=$?
assert_eq "$rc" 0 "disabled exits healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$out")" false "disabled reports enabled=false"

# ── healthy: every expected image current in the tenant store ────────────────
cat >"$registry" <<'JSON'
{"version":17,"tenants":{"acorn":{"enable_voice":false,"vision":{"mode":"local"},"workers":{"pebble":true}}}}
JSON
set_images admin v1 - p1
set_images tenant v1 - p1
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 0 "current images exit healthy"
assert_eq "$(jq -r '.status' <<<"$out")" healthy "current images status healthy"
assert_eq "$(jq -r '.metrics.target_count' <<<"$out")" 1 "one tenant targeted"
assert_eq "$(jq -r '[.metrics.instances[0].images[].state] | join(",")' <<<"$out")" \
  "current,current" "both expected images current"

# ── critical: the WI#271 case — worker image missing from the tenant store ──
set_images admin v1 - p1
set_images tenant v1 - -
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "missing worker image exits critical"
assert_eq "$(jq -r '.status' <<<"$out")" critical "missing worker image status critical"
assert_has "$(jq -r '.issues[]' <<<"$out")" "lunarwing-worker-pebble:latest" \
  "missing-image issue names the worker image"
assert_has "$(jq -r '.issues[]' <<<"$out")" "build-pebble-worker" \
  "missing-image issue names the recovery verb"

# ── degraded: tenant store holds a stale image ID ────────────────────────────
set_images admin v2 - p2
set_images tenant v1 - p1
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 1 "stale images exit degraded"
assert_eq "$(jq -r '.status' <<<"$out")" degraded "stale images status degraded"
assert_eq "$(jq -r '[.metrics.instances[0].images[].state] | join(",")' <<<"$out")" \
  "stale,stale" "stale images reported per image"

# ── tenant-only image (admin store lacks it) is not drift ────────────────────
set_images admin - - -
set_images tenant v1 - p1
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 0 "tenant-only images exit healthy"
assert_eq "$(jq -r '[.metrics.instances[0].images[].state] | join(",")' <<<"$out")" \
  "present-no-admin-copy,present-no-admin-copy" "tenant-only images reported without comparison"

# ── tenants with no expected images are not targets ──────────────────────────
cat >"$registry" <<'JSON'
{"version":17,"tenants":{"nut":{"enable_voice":false,"vision":{"mode":"external"},"workers":{}}}}
JSON
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 0 "no expected images exits healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$out")" false "no expected images reports disabled"
assert_eq "$(jq -r '.metrics.reason' <<<"$out")" no_expected_images "reason recorded"

# ── voice expectation follows enable_voice ───────────────────────────────────
cat >"$registry" <<'JSON'
{"version":17,"tenants":{"acorn":{"enable_voice":true,"vision":{"mode":"local"},"workers":{}}}}
JSON
set_images admin v1 x1 -
set_images tenant v1 - -
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "enabled-but-absent voice image exits critical"
assert_has "$(jq -r '.issues[]' <<<"$out")" "lunarwing/voice-service:latest" \
  "missing voice image named"

# ── malformed registry / invalid flags ───────────────────────────────────────
printf '{not-json\n' >"$registry"
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "malformed registry exits critical"
assert_has "$(jq -r '.issues[]' <<<"$out")" "valid tenant JSON" "malformed registry issue is explicit"

cat >"$registry" <<'JSON'
{"version":17,"tenants":{"acorn":{"enable_voice":true,"vision":{"mode":"local"},"workers":{"pebble":"yes"}}}}
JSON
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "non-boolean worker flag exits critical"
assert_has "$(jq -r '.issues[]' <<<"$out")" "acorn" "invalid-flag issue names tenant"

# ── podman absent: auto disables, explicit true is a monitoring gap ──────────
NOBIN="$ROOT/nobin"; mkdir -p "$NOBIN"
for tool in jq date timeout paste env bash tr cat rm mkdir chmod dirname; do
  ln -s "$(command -v "$tool")" "$NOBIN/$tool"
done
cat >"$registry" <<'JSON'
{"version":17,"tenants":{"acorn":{"enable_voice":false,"vision":{"mode":"local"},"workers":{"pebble":true}}}}
JSON
out="$(PATH="$NOBIN" HEALTH_WORKER_IMAGES_ENABLED=true SELF_HEAL_TENANTS_FILE="$registry" bash "$CHECK" 2>/dev/null)"; rc=$?
assert_eq "$rc" 2 "podman absent with explicit enable exits critical"
out="$(PATH="$NOBIN" SELF_HEAL_TENANTS_FILE="$registry" bash "$CHECK" 2>/dev/null)"; rc=$?
assert_eq "$rc" 0 "podman absent on auto exits healthy"
assert_eq "$(jq -r '.metrics.reason' <<<"$out")" podman_absent "podman_absent reason recorded"

printf '%s passed, %s failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
