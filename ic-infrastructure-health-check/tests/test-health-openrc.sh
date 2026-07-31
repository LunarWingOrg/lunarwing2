#!/usr/bin/env bash
# Test suite for health-openrc.sh — the OpenRC service health probe whose JSON
# report the self-heal engine consumes (.metrics.services[].name/.status).
#
# Self-contained: a fake $INITD_DIR of stub init scripts + a PATH-shadowed mock
# `rc-service` driven by per-service fixtures. Never touches the host's real
# /etc/init.d or services — safe to run anywhere (still prefer a test box).
#
# Each scenario doubles as a regression guard for one of the OpenRC-leg fixes:
#   Rank 10 — real rc_exit capture (ambiguous-but-down no longer reads healthy)
#   Rank  5 — per-unit status timeout (one wedged tenant can't blank the fleet)
#   Rank 18 — discovery dedup (lunarwing-proxy-<t> listed once) + weechat breadth
#   Rank 33 — the dead `pid` field is gone

set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HC="${HEALTH_OPENRC_SCRIPT:-$DIR/../health-openrc.sh}"

[[ -r "$HC" ]] || { echo "FATAL: health-openrc.sh not found at $HC"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }
HAVE_TIMEOUT=1; command -v timeout >/dev/null 2>&1 || HAVE_TIMEOUT=0

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/health-openrc-test.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT

pass=0; fail=0
ok()  { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }
assert_eq() { if [[ "$1" == "$2" ]]; then ok "$3"; else bad "$3" "got [$1] want [$2]"; fi; }
assert_json() { if jq -e . >/dev/null 2>&1 <<<"$1"; then ok "$2"; else bad "$2" "invalid JSON: $1"; fi; }

# A scenario sandbox: $s/initd (fake /etc/init.d), $s/fix (status fixtures),
# $s/bin/rc-service (mock). Echoes $s.
new_scenario() {
  local s; s="$(mktemp -d "$ROOT/s.XXXXXX")"
  mkdir -p "$s/initd" "$s/fix" "$s/bin" "$s/runlevels/default"
  cat >"$s/bin/rc-service" <<'MOCK'
#!/usr/bin/env bash
# Mock OpenRC rc-service: honours $INITD_DIR for --exists and $RC_FIX for status.
INITD_DIR="${INITD_DIR:-/etc/init.d}"; FIX="${RC_FIX:-}"
if [ "${1:-}" = "--exists" ]; then
  [ -x "$INITD_DIR/${2:-}" ] && exit 0 || exit 1
fi
svc="${1:-}"; cmd="${2:-}"
if [ "$cmd" = "status" ]; then
  [ -n "$FIX" ] && [ -f "$FIX/$svc.sleep" ] && sleep "$(cat "$FIX/$svc.sleep")"
  [ -n "$FIX" ] && [ -f "$FIX/$svc.out" ] && cat "$FIX/$svc.out"
  rc=0; [ -n "$FIX" ] && [ -f "$FIX/$svc.rc" ] && rc="$(cat "$FIX/$svc.rc")"
  exit "$rc"
fi
exit 0
MOCK
  chmod +x "$s/bin/rc-service"
  printf '%s' "$s"
}

mk_unit() { : >"$1/initd/$2"; chmod +x "$1/initd/$2"; }                  # <s> <name>
# Mark a unit as enabled in the default runlevel — i.e. the tenant was start-ed
# (start-tenant `rc-update add`s the primary daemon). <s> <name>
enable_unit() { : >"$1/runlevels/default/$2"; }
# Write a fake ports registry for the weechat health-gate. <s> <tenant> <true|false|absent>
mk_registry() {
  if [ "$3" = absent ]; then
    jq -n --arg t "$2" '{tenants:{($t):{user:$t}}}' >"$1/ports.json"
  else
    jq -n --arg t "$2" --argjson w "$3" '{tenants:{($t):{user:$t, enable_weechat:$w}}}' >"$1/ports.json"
  fi
}
# <s> <svc> <status-text> <exit-code> [sleep-secs]
fix_set() {
  printf '%s' "$3" >"$1/fix/$2.out"
  printf '%s' "$4" >"$1/fix/$2.rc"
  [ -n "${5:-}" ] && printf '%s' "$5" >"$1/fix/$2.sleep"
  return 0
}

# run_hc <s> [ENV=VAL ...] -> sets OUT (stdout+stderr) and RC (exit code).
run_hc() {
  local s="$1"; shift
  OUT="$(env PATH="$s/bin:$PATH" INITD_DIR="$s/initd" RUNLEVELS_DIR="$s/runlevels" RC_FIX="$s/fix" SERVICES='' "$@" bash "$HC" 2>&1)"
  RC=$?
}
svc_field() { jq -r --arg n "$2" '.metrics.services[] | select(.name==$n) | .'"$3" <<<"$1"; }

# ── S1: discovery dedup + breadth (Rank 18) + no pid field (Rank 33) ──────────
s="$(new_scenario)"
for u in lunarwing-acme lunarwing-proxy-acme lunarwing-pg-acme \
         lunarwing-weechat-acme lunarwing-weechat-adapter-acme xmpp-bridge-acme; do
  mk_unit "$s" "$u"; fix_set "$s" "$u" " * status: started" 0
done
run_hc "$s"
assert_json "$OUT" "S1: emits valid JSON"
assert_eq "$(jq '[.metrics.services[]|select(.name=="lunarwing-proxy-acme")]|length' <<<"$OUT")" 1 \
  "S1: lunarwing-proxy-acme listed exactly once (dedup, Rank 18)"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-acme name)" "lunarwing-weechat-acme" \
  "S1: weechat discovered via lunarwing-* glob (Rank 18)"
assert_eq "$(svc_field "$OUT" xmpp-bridge-acme name)" "xmpp-bridge-acme" "S1: xmpp-bridge discovered"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "S1: overall healthy"
assert_eq "$RC" 0 "S1: exit 0"
if jq -e '.metrics.services | all(has("pid") | not)' >/dev/null 2>&1 <<<"$OUT"; then
  ok "S1: dead pid field removed (Rank 33)"
else bad "S1: dead pid field removed (Rank 33)" "a service object still has a pid key"; fi

# ── S2: ambiguous output + non-zero exit → critical (Rank 10) ────────────────
s="$(new_scenario)"
mk_unit "$s" lunarwing-acme;       fix_set "$s" lunarwing-acme " * status: started" 0
mk_unit "$s" lunarwing-proxy-acme; fix_set "$s" lunarwing-proxy-acme " * weird diagnostic output" 3
enable_unit "$s" lunarwing-acme    # tenant 'acme' is start-ed → its down units are real outages (F3 gate)
run_hc "$s"
assert_eq "$(svc_field "$OUT" lunarwing-proxy-acme state)"  "stopped"  "S2: ambiguous+nonzero → state stopped (Rank 10)"
assert_eq "$(svc_field "$OUT" lunarwing-proxy-acme status)" "critical" "S2: ambiguous+nonzero → critical (Rank 10)"
assert_eq "$(jq -r '.status' <<<"$OUT")" "critical" "S2: overall critical"
assert_eq "$RC" 2 "S2: exit 2"

# ── S3: ambiguous output but exit 0 → started (fallback preserved) ───────────
s="$(new_scenario)"
mk_unit "$s" lunarwing-acme; fix_set "$s" lunarwing-acme " * (no keyword here)" 0
run_hc "$s"
assert_eq "$(svc_field "$OUT" lunarwing-acme state)" "started" "S3: non-keyword + exit0 → started (fallback intact)"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "S3: overall healthy"

# ── S4: per-unit timeout isolation (Rank 5) ──────────────────────────────────
if [[ "$HAVE_TIMEOUT" -eq 1 ]]; then
  s="$(new_scenario)"
  mk_unit "$s" lunarwing-acme;    fix_set "$s" lunarwing-acme " * status: started" 0
  mk_unit "$s" lunarwing-pg-acme; fix_set "$s" lunarwing-pg-acme " * status: started" 0 3   # hang 3s
  start=$SECONDS
  run_hc "$s" HEALTH_OPENRC_STATUS_TIMEOUT=1
  dur=$((SECONDS - start))
  assert_eq "$(svc_field "$OUT" lunarwing-pg-acme state)"  "timeout"  "S4: hung probe → state timeout (Rank 5)"
  assert_eq "$(svc_field "$OUT" lunarwing-pg-acme status)" "degraded" "S4: hung probe → degraded (Rank 5)"
  assert_eq "$(svc_field "$OUT" lunarwing-acme status)"    "healthy"  "S4: healthy unit still reported (fleet not blanked)"
  assert_eq "$(jq -r '.status' <<<"$OUT")" "degraded" "S4: overall degraded"
  if [[ "$dur" -le 6 ]]; then ok "S4: bounded by per-unit timeout (${dur}s, not the 3s hang × units)"
  else bad "S4: bounded by per-unit timeout" "run took ${dur}s"; fi
else
  echo "SKIP: S4 per-unit timeout test (no 'timeout' binary)"
fi

# ── S5: missing unit → critical ──────────────────────────────────────────────
s="$(new_scenario)"
run_hc "$s" SERVICES=ghost
assert_eq "$(svc_field "$OUT" ghost status)" "critical" "S5: missing unit → critical"
assert_eq "$RC" 2 "S5: exit 2"

# ── S6: cleanly stopped unit on a STARTED tenant → critical ──────────────────
s="$(new_scenario)"
mk_unit "$s" lunarwing-acme; fix_set "$s" lunarwing-acme " * status: stopped" 3
enable_unit "$s" lunarwing-acme    # started tenant → a stopped primary is a real outage
run_hc "$s"
assert_eq "$(svc_field "$OUT" lunarwing-acme status)" "critical" "S6: stopped (started tenant) → critical"
assert_eq "$(jq -r '.status' <<<"$OUT")" "critical" "S6: overall critical"

# ── S7: NOT-yet-started tenant (no runlevel entry) → skipped, not critical (F3/O5) ──
# A tenant that was add-ed (units rendered) but never start-ed: its down units must
# NOT flip the host report to critical, and self-heal must not auto-start them.
s="$(new_scenario)"
mk_unit "$s" lunarwing-fresh;       fix_set "$s" lunarwing-fresh       " * status: stopped" 3
mk_unit "$s" lunarwing-proxy-fresh; fix_set "$s" lunarwing-proxy-fresh " * status: stopped" 3
mk_unit "$s" lunarwing-weechat-adapter-fresh; fix_set "$s" lunarwing-weechat-adapter-fresh " * status: stopped" 3
# (deliberately NOT enabled — tenant 'fresh' is mid-provision)
run_hc "$s"
assert_eq "$(svc_field "$OUT" lunarwing-fresh status)"       "skipped" "S7: not-started tenant primary → skipped (F3/O5)"
assert_eq "$(svc_field "$OUT" lunarwing-proxy-fresh status)" "skipped" "S7: not-started tenant sub-unit → skipped (F3/O5)"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-adapter-fresh status)" "skipped" "S7: weechat-adapter role maps to tenant → skipped"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "S7: overall stays healthy (not flipped to critical)"
assert_eq "$RC" 0 "S7: exit 0"

# ── S8: started tenant's down unit stays critical alongside a skipped one (per-tenant gate) ──
s="$(new_scenario)"
mk_unit "$s" lunarwing-live;       fix_set "$s" lunarwing-live       " * status: started" 0; enable_unit "$s" lunarwing-live
mk_unit "$s" lunarwing-proxy-live; fix_set "$s" lunarwing-proxy-live " * status: stopped" 3
mk_unit "$s" lunarwing-fresh;      fix_set "$s" lunarwing-fresh      " * status: stopped" 3   # not enabled
run_hc "$s"
assert_eq "$(svc_field "$OUT" lunarwing-proxy-live status)" "critical" "S8: started tenant's down unit → critical"
assert_eq "$(svc_field "$OUT" lunarwing-fresh status)"      "skipped"  "S8: not-started tenant's down unit → skipped"
assert_eq "$(jq -r '.status' <<<"$OUT")" "critical" "S8: overall critical (driven by the started tenant)"
assert_eq "$RC" 2 "S8: exit 2"

# ── S9: weechat-DISABLED tenant (enable_weechat=false) — down weechat units → skipped ──
# The render-units footgun scenario: re-rendered optional weechat units that cannot
# start must not read critical (and self-heal must not restart-storm them) when the
# operator disabled weechat for the tenant.
s="$(new_scenario)"
mk_unit "$s" lunarwing-acme;                    fix_set "$s" lunarwing-acme                    " * status: started" 0; enable_unit "$s" lunarwing-acme
mk_unit "$s" lunarwing-proxy-acme;              fix_set "$s" lunarwing-proxy-acme              " * status: stopped" 3
mk_unit "$s" lunarwing-weechat-acme;            fix_set "$s" lunarwing-weechat-acme            " * status: stopped" 3
mk_unit "$s" lunarwing-weechat-adapter-acme;    fix_set "$s" lunarwing-weechat-adapter-acme    " * status: stopped" 3
mk_registry "$s" acme false
run_hc "$s" LUNARWING_TENANTS_FILE="$s/ports.json"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-acme status)"         "skipped"  "S9: disabled tenant's down weechat → skipped"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-adapter-acme status)" "skipped"  "S9: disabled tenant's down weechat-adapter → skipped"
assert_eq "$(svc_field "$OUT" lunarwing-proxy-acme status)"           "critical" "S9: non-weechat down unit still critical (gate is weechat-only)"
assert_eq "$(jq -r '.status' <<<"$OUT")" "critical" "S9: overall critical (driven by proxy, not weechat)"
assert_eq "$RC" 2 "S9: exit 2"

# ── S10: disabled weechat is the ONLY thing down → host stays healthy (flap fixed) ──
s="$(new_scenario)"
mk_unit "$s" lunarwing-acme;                 fix_set "$s" lunarwing-acme                 " * status: started" 0; enable_unit "$s" lunarwing-acme
mk_unit "$s" lunarwing-weechat-acme;         fix_set "$s" lunarwing-weechat-acme         " * status: stopped" 3
mk_unit "$s" lunarwing-weechat-adapter-acme; fix_set "$s" lunarwing-weechat-adapter-acme " * status: crashed" 1
mk_registry "$s" acme false
run_hc "$s" LUNARWING_TENANTS_FILE="$s/ports.json"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-acme status)"         "skipped" "S10: stopped weechat on disabled tenant → skipped"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-adapter-acme status)" "skipped" "S10: crashed weechat-adapter on disabled tenant → skipped"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "S10: overall healthy — the render-units footgun is closed"
assert_eq "$RC" 0 "S10: exit 0"

# ── S11: gate inert when flag absent/true or registry unreadable (fail safe) ──
s="$(new_scenario)"
mk_unit "$s" lunarwing-acme;         fix_set "$s" lunarwing-acme         " * status: started" 0; enable_unit "$s" lunarwing-acme
mk_unit "$s" lunarwing-weechat-acme; fix_set "$s" lunarwing-weechat-acme " * status: stopped" 3
mk_registry "$s" acme absent   # pre-flag tenant entry: no enable_weechat key
run_hc "$s" LUNARWING_TENANTS_FILE="$s/ports.json"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-acme status)" "critical" "S11: flag absent → down weechat still critical (backward compat)"

s="$(new_scenario)"
mk_unit "$s" lunarwing-acme;         fix_set "$s" lunarwing-acme         " * status: started" 0; enable_unit "$s" lunarwing-acme
mk_unit "$s" lunarwing-weechat-acme; fix_set "$s" lunarwing-weechat-acme " * status: stopped" 3
mk_registry "$s" acme true
run_hc "$s" LUNARWING_TENANTS_FILE="$s/ports.json"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-acme status)" "critical" "S11: flag true → down weechat still critical"

s="$(new_scenario)"
mk_unit "$s" lunarwing-acme;         fix_set "$s" lunarwing-acme         " * status: started" 0; enable_unit "$s" lunarwing-acme
mk_unit "$s" lunarwing-weechat-acme; fix_set "$s" lunarwing-weechat-acme " * status: stopped" 3
run_hc "$s" LUNARWING_TENANTS_FILE="$s/nonexistent.json"
assert_eq "$(svc_field "$OUT" lunarwing-weechat-acme status)" "critical" "S11: unreadable registry → down weechat still critical (never masks)"

# ── S12: retired Nanocode units are ignored even when failed ─────────────────
s="$(new_scenario)"
mk_unit "$s" lunarwing-acme; fix_set "$s" lunarwing-acme " * status: started" 0; enable_unit "$s" lunarwing-acme
mk_unit "$s" lunarwing-nanocode-acme; fix_set "$s" lunarwing-nanocode-acme " * status: crashed" 1
mk_unit "$s" lunarwing-nanocode-acme-sup; fix_set "$s" lunarwing-nanocode-acme-sup " * status: stopped" 3
run_hc "$s"
assert_eq "$(jq '[.metrics.services[] | select(.name | startswith("lunarwing-nanocode-"))] | length' <<<"$OUT")" 0 \
  "S12: retired Nanocode units absent from report"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "S12: retired Nanocode failures do not affect overall health"
assert_eq "$RC" 0 "S12: retired Nanocode failures keep exit 0"

run_hc "$s" SERVICES='lunarwing-nanocode-acme lunarwing-nanocode-acme-sup'
assert_eq "$(jq '[.metrics.services[] | select(.name | startswith("lunarwing-nanocode-"))] | length' <<<"$OUT")" 0 \
  "S13: explicit retired Nanocode units absent from report"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "S13: explicit retired Nanocode failures do not affect overall health"
assert_eq "$RC" 0 "S13: explicit retired Nanocode failures keep exit 0"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
