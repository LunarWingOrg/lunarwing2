#!/usr/bin/env bash
# Tests for health-runtime.sh — tenant-aware runtime diagnostics (WI#180 F7).
#
# Self-contained: a fake ports registry + a PATH-shadowed mock curl driven by
# fixture files. Never touches the host's real gateways or bridges.
#
#   A: registry discovery builds one target per tenant from gateway/bridge ports
#   B: unreachable gateway -> critical; pending gates are a metric, not a failure
#   C: reachable gateway + bridge backlog -> degraded/critical by threshold
#   D: bridge unreachable on a healthy gateway -> degraded
#   E: no registry and no base env -> healthy/disabled (not_configured)
#   F: invalid registry -> degraded (monitoring gap, never a false healthy)
#   G: slow fleet keeps partial results (sweep deadline)
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
HC="$DIR/../health-runtime.sh"

[[ -r "$HC" ]] || { echo "FATAL: health-runtime.sh not found at $HC"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

new_sb() {
    local sb; sb="$(mktemp -d "${TMPDIR:-/tmp}/health-runtime-test.XXXXXX")"
    mkdir -p "$sb/bin" "$sb/fix"
    cat >"$sb/bin/curl" <<'EOF'
#!/usr/bin/env bash
# Mock curl: serve $RT_FIX/<path-slug>.body or fail like -s does. An optional
# $RT_FIX/<path-slug>.sleep delays the response (slow-endpoint fixture).
a=("$@")
url=""
for arg in "${a[@]}"; do
  case "$arg" in
    http://*) url="$arg" ;;
  esac
done
slug="$(printf '%s' "$url" | sed 's|http://||; s|[/?]|_|g')"
[ -n "${RT_FIX:-}" ] && [ -f "$RT_FIX/$slug.sleep" ] && sleep "$(cat "$RT_FIX/$slug.sleep")"
[ -n "${RT_FIX:-}" ] && [ -f "$RT_FIX/$slug.body" ] && { cat "$RT_FIX/$slug.body"; exit 0; }
exit 7
EOF
    chmod +x "$sb/bin/curl"
    printf '%s' "$sb"
}

# fix_body <sb> <url> <json>
fix_body() { printf '%s' "$3" > "$1/fix/$(printf '%s' "$2" | sed 's|http://||; s|[/?]|_|g').body"; }
# fix_sleep <sb> <url> <secs> — slow-endpoint fixture (delay before response)
fix_sleep() { printf '%s' "$3" > "$1/fix/$(printf '%s' "$2" | sed 's|http://||; s|[/?]|_|g').sleep"; }
# mk_registry <sb> <tenant> <user> <gw-port> <bridge-port> [extra-tenant-json]
mk_registry() {
    jq -n --arg t "$2" --arg u "$3" --argjson gw "$4" --argjson br "$5" \
      '{tenants:{($t):{user:$u,ports:{gateway:$gw,bridge:$br}}}}' > "$1/ports.json"
}

# run_hc <sb> [ENV=VALUE ...] -> stdout+stderr, rc via $?
run_hc() {
    local sb="$1"; shift
    env PATH="$sb/bin:$PATH" RT_FIX="$sb/fix" HEALTH_RUNTIME_TENANTS_FILE="$sb/none.json" "$@" bash "$HC" 2>&1
}

# ── A: registry discovery + healthy tenant ───────────────────────────────────
a="$(new_sb)"; mk_registry "$a" acme acme 3001 8788
fix_body "$a" "http://127.0.0.1:3001/api/gateway/status" \
  '{"uptime_secs":120,"total_connections":3,"llm_backend":"tensorzero","channel_health":{"xmpp":{"healthy":true}}}'
fix_body "$a" "http://127.0.0.1:3001/api/chat/pending-gates" '{"gates":[]}'
fix_body "$a" "http://127.0.0.1:8788/v1/status" \
  '{"configured":true,"running":true,"queued_messages":2,"outbound_messages_last_hour":5,"omemo_enabled":true,"prekeys_available":90}'
oA="$(run_hc "$a" LUNARWING_TENANTS_FILE="$a/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oA")" "healthy" "A1: reachable gateway + bridge -> healthy"
assert_eq "$(jq -r '.metrics.discovery' <<<"$oA")" "ports-registry" "A2: registry discovery"
assert_eq "$(jq -r '.metrics.target_count' <<<"$oA")" "1" "A3: one tenant target"
assert_eq "$(jq -r '.metrics.instances[0].gateway_uptime_secs' <<<"$oA")" "120" "A4: gateway uptime surfaced"
assert_eq "$(jq -r '.metrics.instances[0].bridge_queued_messages' <<<"$oA")" "2" "A5: bridge queue surfaced"
assert_eq "$(jq -r '.metrics.instances[0].bridge_omemo_enabled' <<<"$oA")" "true" "A6: OMEMO diagnostics surfaced"

# ── B: unreachable gateway -> critical; pending gates are a metric only ──────
b="$(new_sb)"; mk_registry "$b" acme acme 3001 8788
# gateway down: no fixtures -> curl fails
oB="$(run_hc "$b" LUNARWING_TENANTS_FILE="$b/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oB")" "critical" "B1: unreachable gateway -> critical"
assert_contains "$(jq -r '.issues[0]' <<<"$oB")" "gateway unreachable" "B2: gateway failure explained"

# ── C: gateway up, bridge backlog thresholds ─────────────────────────────────
c="$(new_sb)"; mk_registry "$c" acme acme 3001 8788
fix_body "$c" "http://127.0.0.1:3001/api/gateway/status" '{"uptime_secs":10,"total_connections":0}'
fix_body "$c" "http://127.0.0.1:3001/api/chat/pending-gates" '{"gates":[{"request_id":"r1"}]}'
fix_body "$c" "http://127.0.0.1:8788/v1/status" '{"running":true,"queued_messages":12}'
oC="$(run_hc "$c" LUNARWING_TENANTS_FILE="$c/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oC")" "degraded" "C1: bridge backlog > degraded threshold"
assert_eq "$(jq -r '.metrics.instances[0].pending_gates' <<<"$oC")" "1" "C2: pending gates surfaced as a metric"
assert_contains "$(jq -r '.issues[0]' <<<"$oC")" "backlog elevated" "C3: backlog issue named"

c2="$(new_sb)"; mk_registry "$c2" acme acme 3001 8788
fix_body "$c2" "http://127.0.0.1:3001/api/gateway/status" '{"uptime_secs":10,"total_connections":0}'
fix_body "$c2" "http://127.0.0.1:8788/v1/status" '{"running":true,"queued_messages":60}'
oC2="$(run_hc "$c2" LUNARWING_TENANTS_FILE="$c2/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oC2")" "critical" "C4: bridge backlog > critical threshold"

# ── D: bridge unreachable on a healthy gateway -> degraded ──────────────────
d="$(new_sb)"; mk_registry "$d" acme acme 3001 8788
fix_body "$d" "http://127.0.0.1:3001/api/gateway/status" '{"uptime_secs":10,"total_connections":0}'
oD="$(run_hc "$d" LUNARWING_TENANTS_FILE="$d/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oD")" "degraded" "D1: bridge unreachable -> degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$oD")" "bridge unreachable" "D2: bridge failure explained"

# ── E: no registry, no base env -> healthy/disabled ─────────────────────────
e="$(new_sb)"
oE="$(run_hc "$e" LUNARWING_BASE_DIR="$e/nonexistent-base" LUNARWING_TENANTS_FILE="$e/none.json" SELF_HEAL_TENANTS_FILE="")"
assert_eq "$(jq -r '.status' <<<"$oE")" "healthy" "E1: unconfigured single node -> healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$oE")" "false" "E2: unconfigured single node -> disabled"

# ── F: invalid registry -> degraded monitoring gap ──────────────────────────
f="$(new_sb)"; printf 'not json' > "$f/ports.json"
oF="$(run_hc "$f" LUNARWING_TENANTS_FILE="$f/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oF")" "degraded" "F1: invalid registry -> degraded, never healthy"

# ── G: slow fleet keeps partial results (sweep deadline) ────────────────────
g="$(new_sb)"
jq -n '{tenants:{acme:{user:"acme",ports:{gateway:3001,bridge:8788}},beta:{user:"beta",ports:{gateway:3002,bridge:8789}},gamma:{user:"gamma",ports:{gateway:3003,bridge:8790}}}}' > "$g/ports.json"
# acme healthy; beta's gateway hangs 4s (under the per-curl timeout, over the
# 3s sweep deadline); gamma is never reached because the deadline passes.
fix_body "$g" "http://127.0.0.1:3001/api/gateway/status" '{"uptime_secs":10,"total_connections":0}'
fix_body "$g" "http://127.0.0.1:3001/api/chat/pending-gates" '{"gates":[]}'
fix_body "$g" "http://127.0.0.1:8788/v1/status" '{"running":true,"queued_messages":0}'
fix_sleep "$g" "http://127.0.0.1:3002/api/gateway/status" 4
start=$SECONDS
oG="$(run_hc "$g" LUNARWING_TENANTS_FILE="$g/ports.json" HEALTH_RUNTIME_SWEEP_DEADLINE_SECS=3 HEALTH_RUNTIME_TIMEOUT=10)"
dur=$((SECONDS - start))
assert_eq "$(jq -r '.metrics.instances[0].tenant' <<<"$oG")" "acme" "G1: completed observation retained"
assert_eq "$(jq -r '.metrics.instances[0].status' <<<"$oG")" "healthy" "G2: completed observation is healthy"
assert_eq "$(jq -r '.metrics.instances[2].status' <<<"$oG")" "unprobed" "G3: unreached target marked unprobed"
# beta's slow gateway resolves to a real critical, which must dominate the
# coverage degradation (never mask a worse verdict).
assert_eq "$(jq -r '.status' <<<"$oG")" "critical" "G4: real critical dominates coverage loss"
assert_contains "$(jq -r '.issues[]' <<<"$oG")" "sweep incomplete" "G5: coverage loss surfaced as an issue"
if [[ "$dur" -le 12 ]]; then ok "G5: run bounded by sweep deadline (${dur}s)"
else bad "G5: run bounded by sweep deadline" "run took ${dur}s"; fi

finish
