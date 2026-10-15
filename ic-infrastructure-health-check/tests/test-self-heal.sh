#!/usr/bin/env bash
# Behavior + regression tests for the merged lunarwing-self-heal.sh.
#
# Covers: jq-precedence regression, exponential backoff + jitter (and the
# linear strategy toggle), grace period, flapping guard, post-restart
# verification, state TTL prune (+ --prune-ttl disable), report-as-truth
# recovery, multi-tenant remediation (systemd user units + OpenRC), and the
# CLI flags. Output routing uses --dry-run; persisted state transitions use the
# mock init system from lib.sh.
#
# Run:  bash tests/test-self-heal.sh   (exit 0 = pass, 1 = fail)

set -uo pipefail
# shellcheck source=tests/lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
# WI#410 ID-2: the guard demands byte-for-byte equality with mt-admin's render,
# so realistic lunarwing-proxy-* fixtures are rendered from the shared contract
# library the script sources. <sb> <tenant>
# shellcheck source=../openrc-proxy-unit-contract.sh
source "$(dirname "$SH")/openrc-proxy-unit-contract.sh"
mk_proxy_unit() { _openrc_proxy_unit_body "$2" 21001 >"$1/initd/lunarwing-proxy-$2"; chmod +x "$1/initd/lunarwing-proxy-$2"; }

# ── A: jq-precedence regression + target selection (grace=1) ────────────────
A="$(sb)"; report "$A" '{components:[
  {component:"gateway",status:"degraded",metrics:{}},
  {component:"omemo",  status:"critical",metrics:{}},
  {component:"systemd",status:"degraded",metrics:{units:[
    {name:"lunarwing-watchdog.service",status:"healthy"},
    {name:"clickhouse-server.service", status:"critical"}]}}]}'
oA="$(run_dry "$A" LUNARWING_SERVICE_MANAGER=systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oA" "systemctl restart clickhouse-server.service" "A: unhealthy systemd sub-unit remediated"
assert_contains "$oA" "systemctl restart lunarwing"                 "A: gateway maps to lunarwing"
assert_absent   "$oA" "restart lunarwing-watchdog.service"          "A: healthy sub-unit not restarted"
assert_absent   "$oA" "restart omemo"                               "A: no-remedy not restarted"
assert_absent   "$oA" "Cannot index string"                        "A: pass-2 jq does not abort"

# ── B: backoff (item 1) ─────────────────────────────────────────────────────
B="$(sb)"; mk_mockbin "$B"; svc_down "$B" lunarwing; svc_stuck "$B" lunarwing; report "$B" "$GW"
B_started="$(now)"
run_chaos "$B" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false >/dev/null
B_finished="$(now)"
nb="$(state_of "$B" '.lunarwing.next_attempt_at')"
assert_range "$nb" "$B_started" "$((B_finished + 60))" "B1: exponential schedules next_attempt_at within the jitter window"
oB2="$(run_chaos "$B" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false)"
# (only asserts the gate when the jittered delay was > 0; if 0 a restart is fine)
remaining=$((nb - $(now)))
if [[ "$remaining" -gt 1 ]]; then
  assert_contains "$oB2" "BACKOFF" "B2: second run gated by backoff"
else ok "B2: backoff was 0 (jitter) — no gate expected"; fi
Bl="$(sb)"; mk_mockbin "$Bl"; svc_down "$Bl" lunarwing; svc_stuck "$Bl" lunarwing; report "$Bl" "$GW"
Bl_started="$(now)"
run_chaos "$Bl" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false \
    SELF_HEAL_BACKOFF_STRATEGY=linear SELF_HEAL_BACKOFF_BASE=300 >/dev/null
Bl_finished="$(now)"
Bl_next="$(state_of "$Bl" '.lunarwing.next_attempt_at')"
assert_range "$Bl_next" "$((Bl_started + 300))" "$((Bl_finished + 300))" "B3: linear strategy is deterministic (now+base)"

# ── C: grace period (item 2) ────────────────────────────────────────────────
C="$(sb)"; mk_mockbin "$C"; svc_down "$C" lunarwing; report "$C" "$GW"
oC1="$(run_chaos "$C" systemd SELF_HEAL_GRACE_CHECKS=2 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oC1" "GRACE" "C1: first observation defers (grace)"
assert_absent   "$oC1" "RESTART: lunarwing" "C1: no restart during grace"
oC2="$(run_chaos "$C" systemd SELF_HEAL_GRACE_CHECKS=2 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oC2" "RESTART: lunarwing" "C2: restart fires once grace met"

# ── D: flapping guard (item 2b) ─────────────────────────────────────────────
D="$(sb)"; report "$D" "$GW"
n="$(now)"; jq -n --argjson n "$n" '{lunarwing:{consecutive_unhealthy:5,restart_history:[($n-10),($n-20),($n-30),($n-40),($n-50)]}}' > "$D/self-heal/state.json"
oD="$(run_dry "$D" LUNARWING_SERVICE_MANAGER=systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_FLAP_MAX_RESTARTS=5 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oD" "FLAPPING" "D1: flapping detected"
assert_contains "$oD" "would send escalation notification for lunarwing" "D2: flapping escalates"
assert_absent   "$oD" "would run: systemctl restart lunarwing" "D3: flapping not restarted"

# ── E: post-restart verification (item 3) ───────────────────────────────────
E="$(sb)"; mk_mockbin "$E"; svc_down "$E" lunarwing; svc_stuck "$E" lunarwing; report "$E" "$GW"
oE1="$(run_chaos "$E" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=true)"
assert_contains "$oE1" "VERIFY: gateway still critical" "E1: verification detects still-unhealthy"
assert_eq "$(state_of "$E" '.lunarwing.retries')" "1"   "E1: failed verify counts as a retry"
E2="$(sb)"; mk_mockbin "$E2"; svc_down "$E2" lunarwing; report "$E2" "$GW"
oE2="$(run_chaos "$E2" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=true)"
assert_contains "$oE2" "SUCCESS: lunarwing healthy after restart" "E2: healthy verify clears the service"
assert_eq "$(state_of "$E2" '.lunarwing.retries // 0')" '0' "E2: retry state cleared after verified-healthy restart"
assert_eq "$(state_of "$E2" '.lunarwing.restart_history | length')" '1' "E2: restart history retained after recovery"

# ── F: state prune (item 4) + --prune-ttl disable ───────────────────────────
F="$(sb)"; report "$F" '{components:[{component:"gateway",status:"healthy",metrics:{}}]}'
o=$(( $(now) - 999999 ))
jq -n --argjson o "$o" '{"old-svc":{retries:1,escalated:false,last_attempt:$o},"old-esc":{retries:3,escalated:true,last_attempt:$o}}' > "$F/self-heal/state.json"
run_raw "$F" LUNARWING_SERVICE_MANAGER=unknown SELF_HEAL_VERIFY_HEALTH=false -- --report "$F/report.json" --backoff 0 >/dev/null
assert_eq "$(state_of "$F" 'keys')" '["old-esc"]' "F1: stale non-escalated pruned, escalated kept"
F2="$(sb)"; report "$F2" '{components:[{component:"gateway",status:"healthy",metrics:{}}]}'
jq -n --argjson o "$o" '{"old-svc":{retries:1,escalated:false,last_attempt:$o}}' > "$F2/self-heal/state.json"
run_raw "$F2" LUNARWING_SERVICE_MANAGER=unknown SELF_HEAL_VERIFY_HEALTH=false -- --report "$F2/report.json" --backoff 0 --prune-ttl 0 >/dev/null
assert_eq "$(state_of "$F2" 'keys')" '["old-svc"]' "F2: --prune-ttl 0 disables pruning"

# ── G: multi-tenant (item 5) ────────────────────────────────────────────────
G="$(sb)"; echo '{"tenants":{"summer":{"user":"summer"}}}' > "$G/ports.json"
report "$G" '{components:[{component:"systemd",status:"degraded",metrics:{units:[{name:"lunarwing-summer.service",status:"critical"}]}}]}'
oG="$(run_dry "$G" LUNARWING_SERVICE_MANAGER=systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false SELF_HEAL_TENANTS_FILE="$G/ports.json")"
assert_contains "$oG" "sudo -n -u summer" "G1: systemd tenant unit restarts as tenant user"
assert_contains "$oG" "systemctl --user restart lunarwing-summer.service" "G2: uses systemd --user bus"
H_oc="$(sb)"; report "$H_oc" '{components:[{component:"openrc",status:"degraded",metrics:{services:[{name:"lunarwing-summer",status:"critical"}]}}]}'
oGo="$(run_dry "$H_oc" LUNARWING_SERVICE_MANAGER=openrc SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oGo" "rc-service lunarwing-summer restart" "G3: OpenRC tenant service via rc-service"

# ── H: report-as-truth recovery ─────────────────────────────────────────────
H="$(sb)"; report "$H" '{components:[{component:"gateway",status:"healthy",metrics:{}}]}'
echo '{"lunarwing":{"retries":1,"consecutive_unhealthy":2}}' > "$H/self-heal/state.json"
oH="$(run_raw "$H" LUNARWING_SERVICE_MANAGER=unknown SELF_HEAL_VERIFY_HEALTH=false -- --report "$H/report.json" --backoff 0)"
assert_contains "$oH" "RECOVERED" "H1: service the report calls healthy is cleared (report-as-truth)"
assert_eq "$(state_of "$H")" '{}' "H1: recovered state cleared without is-active round-trip"

# ── I: CLI flags ────────────────────────────────────────────────────────────
I1="$(sb)"; report "$I1" "$GW"
oI1="$(run_dry "$I1" LUNARWING_SERVICE_MANAGER=systemd SELF_HEAL_VERIFY_HEALTH=false -- --grace-checks 1)"
assert_contains "$oI1" "would run: systemctl restart lunarwing" "I1: --grace-checks 1 restarts on first observation"
I2="$(sb)"; mk_mockbin "$I2"; svc_down "$I2" lunarwing; svc_stuck "$I2" lunarwing; report "$I2" "$GW"
I2_started="$(now)"
run_chaos "$I2" systemd SELF_HEAL_VERIFY_HEALTH=false -- --grace-checks 1 --backoff-strategy linear --backoff-base 200 >/dev/null
I2_finished="$(now)"
I2_next="$(state_of "$I2" '.lunarwing.next_attempt_at')"
assert_range "$I2_next" "$((I2_started + 200))" "$((I2_finished + 200))" "I2: --backoff-strategy/-base flags honored"
I3="$(sb)"; mk_mockbin "$I3"; svc_down "$I3" lunarwing; svc_stuck "$I3" lunarwing; report "$I3" "$GW"
run_chaos "$I3" systemd SELF_HEAL_GRACE_CHECKS=1 -- --verify-health false >/dev/null
assert_eq "$(state_of "$I3" '.lunarwing.retries')" "1" "I3: --verify-health false skips health-check (is-active only)"

# ── J: WI#410 runtime safe-unit guard (OpenRC migration window) ──────────────
# An unsafe legacy proxy unit must refuse the WHOLE root-cron tick — no probe,
# no restart, no state mutation — while a rendered (safe) unit keeps normal
# remediation and the guard never fires under systemd.
J="$(sb)"; mkdir -p "$J/initd"
printf ': "${proxy_args:=/opt/x/lunarwing-proxy.py --port 21001 --tensorzero http://tz.example/tz?tok=abc}"\n' \
  > "$J/initd/lunarwing-proxy-acme"; chmod +x "$J/initd/lunarwing-proxy-acme"
report "$J" '{components:[{component:"openrc",status:"degraded",metrics:{services:[{name:"lunarwing-summer",status:"critical"}]}}]}'
oJ="$(run_raw "$J" LUNARWING_SERVICE_MANAGER=openrc LUNARWING_MT_OPENRC_INITD_DIR="$J/initd" \
  SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false -- --report "$J/report.json" --backoff 0)"; RC=$?
assert_contains "$oJ" "unsafe legacy proxy unit"          "J1: legacy proxy unit refuses the tick"
assert_contains "$oJ" "render-units --all-proxies"        "J1: refusal names the remediation"
assert_absent   "$oJ" "rc-service"                        "J1: no rc-service probe or restart runs"
assert_absent   "$oJ" "RESTART"                           "J1: tick aborted before any remediation"
assert_eq "$RC" 1 "J1: exit 1 (fail closed)"
assert_eq "$(state_of "$J")" "" "J1: no state written"

J2="$(sb)"; mkdir -p "$J2/initd"
mk_proxy_unit "$J2" acme
report "$J2" '{components:[{component:"openrc",status:"degraded",metrics:{services:[{name:"lunarwing-summer",status:"critical"}]}}]}'
oJ2="$(run_dry "$J2" LUNARWING_SERVICE_MANAGER=openrc LUNARWING_MT_OPENRC_INITD_DIR="$J2/initd" \
  SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oJ2" "rc-service lunarwing-summer restart" "J2: safe rendered proxy unit keeps normal remediation"

J3="$(sb)"; mkdir -p "$J3/initd"
printf ': "${proxy_args:=/opt/x/lunarwing-proxy.py --port 21001 --tensorzero http://tz.example/tz?tok=abc}"\n' \
  > "$J3/initd/lunarwing-proxy-acme"; chmod +x "$J3/initd/lunarwing-proxy-acme"
report "$J3" "$GW"
oJ3="$(run_dry "$J3" LUNARWING_SERVICE_MANAGER=systemd LUNARWING_MT_OPENRC_INITD_DIR="$J3/initd" \
  SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oJ3" "would run: systemctl restart lunarwing" "J3: guard is OpenRC-only (systemd tick unaffected)"

# Unit level (src_fn): the helper reports the first offender — a symlinked
# proxy unit fails the contract even when its target is a safe rendered unit.
J4="$(sb)"; mkdir -p "$J4/initd"
mk_proxy_unit "$J4" ok
assert_eq "$(src_fn LUNARWING_MT_OPENRC_INITD_DIR="$J4/initd" -- first_unsafe_openrc_proxy_unit "$J4/initd")" "" \
  "J4: rendered safe units pass the guard"
# J5's link target is a same-name unit that would pass as a regular file, so
# the refusal pins the `! -L` check itself (run-126 ID-3 shape).
mkdir -p "$J4/outside"; _openrc_proxy_unit_body link 21001 >"$J4/outside/lunarwing-proxy-link"; chmod +x "$J4/outside/lunarwing-proxy-link"
ln -s "$J4/outside/lunarwing-proxy-link" "$J4/initd/lunarwing-proxy-link"
assert_eq "$(src_fn LUNARWING_MT_OPENRC_INITD_DIR="$J4/initd" -- first_unsafe_openrc_proxy_unit "$J4/initd")" \
  "$J4/initd/lunarwing-proxy-link" "J5: symlinked proxy unit is the first offender"

# ── J6: WI#410 ID-1 — a persisting refusal pages once per cooldown ──────────
# HEALTHCHECK_NOTIFY is false on MT hosts and the health report carries no
# restart target, so the refusal itself must escalate: one page, a durable
# ESCALATE line in actions.log, no state.json, and a second tick inside the
# cooldown is deferred — still exit 1, still no rc-service.
J6="$(sb)"; mkdir -p "$J6/initd" "$J6/bin"
printf ': "${proxy_args:=/opt/x/lunarwing-proxy.py --port 21001 --tensorzero http://tz.example/tz?tok=abc}"\n' \
  > "$J6/initd/lunarwing-proxy-acme"; chmod +x "$J6/initd/lunarwing-proxy-acme"
printf '#!/usr/bin/env bash\necho "notified $1" >>"%s/pages"\nexit 0\n' "$J6" > "$J6/bin/notify"; chmod +x "$J6/bin/notify"
report "$J6" '{components:[{component:"openrc",status:"degraded",metrics:{services:[{name:"lunarwing-summer",status:"critical"}]}}]}'
oJ6="$(run_raw "$J6" LUNARWING_SERVICE_MANAGER=openrc LUNARWING_MT_OPENRC_INITD_DIR="$J6/initd" \
  SELF_HEAL_NOTIFY_SCRIPT="$J6/bin/notify" SELF_HEAL_ESCALATE_COOLDOWN=3600 \
  SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false -- --report "$J6/report.json" --backoff 0)"; RC=$?
assert_eq "$RC" 1 "J6: refusal exits 1"
assert_contains "$oJ6" "unsafe legacy proxy unit"            "J6: refusal names the unit"
assert_contains "$oJ6" "ESCALATING: openrc-proxy-units"      "J6: refusal escalates"
assert_eq "$(wc -l <"$J6/pages" 2>/dev/null | tr -d ' ')" "1" "J6: exactly one page sent"
assert_contains "$(cat "$J6/self-heal/actions.log" 2>/dev/null)" "ESCALATE target=openrc-proxy-units" "J6: durable ESCALATE line in actions.log"
assert_eq "$(state_of "$J6")" "" "J6: still no state.json"
assert_absent   "$oJ6" "rc-service"                          "J6: no rc-service around the page"
oJ6b="$(run_raw "$J6" LUNARWING_SERVICE_MANAGER=openrc LUNARWING_MT_OPENRC_INITD_DIR="$J6/initd" \
  SELF_HEAL_NOTIFY_SCRIPT="$J6/bin/notify" SELF_HEAL_ESCALATE_COOLDOWN=3600 \
  SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false -- --report "$J6/report.json" --backoff 0)"; RC=$?
assert_eq "$RC" 1 "J6: second tick still refuses"
assert_contains "$oJ6b" "COOLDOWN: page for openrc-proxy-units deferred" "J6: second tick is cooldown-deferred"
assert_eq "$(wc -l <"$J6/pages" | tr -d ' ')" "1" "J6: no second page inside the cooldown"
assert_eq "$(state_of "$J6")" "" "J6: second tick writes no state.json either"

# ── J7: WI#410 ID-2 — a missing contract library is itself a refusal ────────
# A hand-copied lunarwing-self-heal.sh without openrc-proxy-unit-contract.sh
# beside it must not probe or restart anything as root; it pages like J6.
J7="$(sb)"; mkdir -p "$J7/copy" "$J7/initd"; cp "$SH" "$J7/copy/lunarwing-self-heal.sh"
report "$J7" '{components:[{component:"openrc",status:"degraded",metrics:{services:[{name:"lunarwing-summer",status:"critical"}]}}]}'
oJ7="$(env LUNARWING_BASE_DIR="$J7" SELF_HEAL_STATE_DIR="$J7/self-heal" GOTIFY_TOKEN='' \
  LUNARWING_SERVICE_MANAGER=openrc LUNARWING_MT_OPENRC_INITD_DIR="$J7/initd" SELF_HEAL_VERIFY_HEALTH=false \
  "$J7/copy/lunarwing-self-heal.sh" --report "$J7/report.json" --backoff 0 2>&1)"; RC=$?
assert_eq "$RC" 1 "J7: missing library refuses (exit 1)"
assert_contains "$oJ7" "openrc-proxy-unit-contract.sh is missing" "J7: refusal names the library and the re-sync"
assert_contains "$oJ7" "ESCALATING: openrc-proxy-units"          "J7: missing library pages"
assert_absent   "$oJ7" "rc-service"                              "J7: no rc-service without the contract"
assert_eq "$(state_of "$J7")" "" "J7: no state written"

# ── J8: WI#410 ID-2 — the cron contract is mt-admin's byte-for-byte contract ─
# The safe flag line plus a payload (accepted by the retired presence grep,
# refused by mt-admin) refuses the whole tick here as well.
J8="$(sb)"; mkdir -p "$J8/initd"
mk_proxy_unit "$J8" acme
echo 'start_post() { cp /root/.ssh/id_ed25519 /tmp/x; }' >>"$J8/initd/lunarwing-proxy-acme"
report "$J8" '{components:[{component:"openrc",status:"degraded",metrics:{services:[{name:"lunarwing-summer",status:"critical"}]}}]}'
oJ8="$(run_dry "$J8" LUNARWING_SERVICE_MANAGER=openrc LUNARWING_MT_OPENRC_INITD_DIR="$J8/initd" \
  SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false)"
assert_contains "$oJ8" "unsafe legacy proxy unit"   "J8: safe line plus payload refuses the tick"
assert_absent   "$oJ8" "rc-service"                 "J8: no rc-service past a payload-carrying unit"

finish
