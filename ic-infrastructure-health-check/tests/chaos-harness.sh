#!/usr/bin/env bash
# Phase 2/3 chaos harness for lunarwing-self-heal.sh — sections 4 & 5 of
# docs/proposals/CHAOS_ENGINEERING_TEST_PLAN.md.
#
# Unlike the dry-run unit matrix, this drives the REAL self-heal loop (no
# --dry-run) end-to-end: it injects a fault, lets self-heal restart/verify, and
# checks the outcome — recovery, escalation, flap-stop, grace-absorption, and
# multi-tenant blast-radius isolation.
#
# It is still safe to run anywhere. Every fault is injected into a *mock init
# system* (fake systemctl/rc-service/sudo + fake component health checks, see
# lib.sh:mk_mockbin) driven entirely by files under a throwaway sandbox. The
# mock shadows the real binaries on PATH, so a "restart" flips a sandbox file —
# never a real unit. Escalation uses a sandbox notifier that confirms delivery
# without any network call.
#
# Each scenario simulates cron ticks by invoking self-heal repeatedly; backoff
# base is 0 so retries are not gated across the simulated ticks.
#
# Run:  bash tests/chaos-harness.sh   (exit 0 = all pass)

set -uo pipefail
# shellcheck source=tests/lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

HEALTHY_GW='{components:[{component:"gateway",status:"healthy",metrics:{}}]}'

echo "=== CH1: kill gateway → restart + verify → recovered ==="
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" lunarwing; report "$c" "$GW"
o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1)"
assert_contains "$o" "SUCCESS: lunarwing healthy after restart" "CH1: gateway recovers after restart"
assert_eq "$(svc_state "$c" lunarwing)"   "up" "CH1: gateway is up post-recovery"
assert_eq "$(restarts_of "$c" lunarwing)" "1"  "CH1: exactly one restart"
assert_eq "$(state_of "$c" '.lunarwing.retries // 0')" "0" "CH1: retry state cleared on recovery"
assert_eq "$(state_of "$c" '.lunarwing.restart_history | length')" "1" "CH1: restart history retained"
assert_eq "$(restarts_of "$c" xmpp-bridge)" "0" "CH1: no collateral restart of other services"

echo "=== CH2: gateway recovers, then stays bound (no churn on next healthy tick) ==="
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" lunarwing; report "$c" "$GW"
run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 >/dev/null            # recover (restart #1)
report "$c" "$HEALTHY_GW"
o2="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1)"               # healthy tick
assert_eq "$(restarts_of "$c" lunarwing)" "1"   "CH2: no second restart once healthy"
assert_absent "$o2" "RESTART: lunarwing"        "CH2: healthy report does no work"

echo "=== CH3: kill xmpp-bridge → recovered ==="
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" xmpp-bridge
report "$c" '{components:[{component:"xmpp",status:"critical",metrics:{}}]}'
o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1)"
assert_contains "$o" "SUCCESS: xmpp-bridge healthy after restart" "CH3: xmpp-bridge recovers"
assert_eq "$(svc_state "$c" xmpp-bridge)" "up" "CH3: xmpp-bridge up"

echo "=== CH4: kill tensorzero-gateway → recovered ==="
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" tensorzero-gateway
report "$c" '{components:[{component:"tensorzero",status:"critical",metrics:{}}]}'
o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1)"
assert_contains "$o" "SUCCESS: tensorzero-gateway healthy after restart" "CH4: tensorzero recovers"
assert_eq "$(svc_state "$c" tensorzero-gateway)" "up" "CH4: tensorzero up"

echo "=== CH5: clickhouse crash → recovered ==="
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" clickhouse-server
report "$c" '{components:[{component:"clickhouse",status:"critical",metrics:{}}]}'
o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1)"
assert_contains "$o" "SUCCESS: clickhouse-server healthy after restart" "CH5: clickhouse recovers"
assert_eq "$(svc_state "$c" clickhouse-server)" "up" "CH5: clickhouse up"

echo "=== CH6: non-recoverable fault (stuck service) → escalates after max retries ==="
# Models C6 (corrupt env) and C7 (DNS failure): a service that restarts but never
# becomes healthy. Self-heal must bound attempts at max-retries, then escalate.
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" lunarwing; svc_stuck "$c" lunarwing; report "$c" "$GW"
last=""
for _ in 1 2 3 4; do
    last="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_MAX_RETRIES=3 SELF_HEAL_BACKOFF_BASE=0)"
done
assert_contains "$last" "ESCALATING: lunarwing"            "CH6: escalates when unrecoverable"
assert_eq "$(state_of "$c" '.lunarwing.escalated')" "true" "CH6: state marked escalated"
assert_eq "$(svc_state "$c" lunarwing)"   "down"           "CH6: service never recovered (stuck)"
assert_eq "$(restarts_of "$c" lunarwing)" "3"              "CH6: restart attempts bounded at max-retries"

echo "=== CH9: restart command keeps failing → escalates (no endless loop) ==="
# Models C9 (port conflict): the restart command itself returns non-zero.
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" lunarwing; svc_fail "$c" lunarwing; report "$c" "$GW"
o1="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_MAX_RETRIES=3 SELF_HEAL_BACKOFF_BASE=0)"
assert_contains "$o1" "FAILURE: restart command failed for lunarwing" "CH9: failed restart is recorded"
last=""
for _ in 2 3 4; do
    last="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_MAX_RETRIES=3 SELF_HEAL_BACKOFF_BASE=0)"
done
assert_contains "$last" "ESCALATING: lunarwing"            "CH9: escalates after repeated restart failures"
assert_eq "$(state_of "$c" '.lunarwing.escalated')" "true" "CH9: state marked escalated"
assert_eq "$(svc_state "$c" lunarwing)" "down"             "CH9: service still down"

echo "=== CH12: rapid flapping → flap guard escalates, restarts capped ==="
# High max-retries so the flap window (not max-retries) is what stops the loop.
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" lunarwing; svc_stuck "$c" lunarwing; report "$c" "$GW"
last=""
for _ in 1 2 3 4 5 6; do
    last="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_MAX_RETRIES=10 \
        SELF_HEAL_FLAP_MAX_RESTARTS=5 SELF_HEAL_FLAP_WINDOW_SECS=3600 SELF_HEAL_BACKOFF_BASE=0)"
done
assert_contains "$last" "FLAPPING: lunarwing"              "CH12: flapping detected"
assert_eq "$(state_of "$c" '.lunarwing.escalated')" "true" "CH12: flapping escalates"
assert_eq "$(restarts_of "$c" lunarwing)" "5"              "CH12: restarts capped at flap threshold (no endless loop)"

echo "=== CH13: transient blip absorbed by the grace period (no restart) ==="
c="$(sb)"; mk_mockbin "$c"; svc_down "$c" lunarwing; report "$c" "$GW"
o1="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=2 SELF_HEAL_BACKOFF_BASE=0)"   # tick 1: defer
svc_up "$c" lunarwing; report "$c" "$HEALTHY_GW"                                   # self-recovers
o2="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=2 SELF_HEAL_BACKOFF_BASE=0)"   # tick 2: healthy
assert_contains "$o1" "GRACE: lunarwing unhealthy 1/2" "CH13: first blip deferred by grace"
assert_eq "$(restarts_of "$c" lunarwing)" "0"          "CH13: transient blip never triggers a restart"
assert_contains "$o2" "RECOVERED"                      "CH13: report-as-truth clears the blip"
assert_eq "$(state_of "$c")" "{}"                      "CH13: state clean after the blip"

echo "=== CH10: tenant unit crash → remediated as the tenant user ==="
c="$(sb)"; mk_mockbin "$c"
me="$(id -un)"
jq -n --arg u "$me" '{tenants:{acme:{user:$u}}}' > "$c/ports.json"
svc_down "$c" "lunarwing-acme.service"
# WI#231: a CRASHED unit reads ActiveState=failed (a SIGKILLed daemon); a merely
# inactive unit of a stopped tenant is deliberately-stopped and is NOT
# remediated. Model the crash so the lifecycle gate (correctly) lets it through;
# the mock restart clears the override so verify sees it healthy.
svc_active "$c" "lunarwing-acme.service" failed
report "$c" '{components:[{component:"systemd",status:"critical",metrics:{units:[
  {name:"lunarwing-acme.service",status:"critical"}]}}]}'
o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_TENANTS_FILE="$c/ports.json")"
assert_contains "$o" "SUCCESS: lunarwing-acme.service healthy after restart" "CH10: tenant unit recovers"
assert_eq "$(svc_state "$c" "lunarwing-acme.service")"   "up" "CH10: tenant unit up"
assert_eq "$(restarts_of "$c" "lunarwing-acme.service")" "1"  "CH10: tenant unit restarted once"
assert_eq "$(state_of "$c" '.["lunarwing-acme.service"].retries // 0')" "0" "CH10: tenant retry state cleared"
assert_eq "$(state_of "$c" '.["lunarwing-acme.service"].restart_history | length')" "1" "CH10: tenant restart history retained"

echo "=== CH11: one tenant down, another healthy → blast-radius isolation ==="
c="$(sb)"; mk_mockbin "$c"
jq -n --arg u "$me" '{tenants:{acme:{user:$u},globex:{user:$u}}}' > "$c/ports.json"
svc_down "$c" "lunarwing-acme.service"
svc_active "$c" "lunarwing-acme.service" failed   # WI#231: crashed, not stopped
svc_up   "$c" "lunarwing-globex.service"
report "$c" '{components:[{component:"systemd",status:"critical",metrics:{units:[
  {name:"lunarwing-acme.service",  status:"critical"},
  {name:"lunarwing-globex.service",status:"healthy"}]}}]}'
o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_TENANTS_FILE="$c/ports.json")"
assert_contains "$o" "SUCCESS: lunarwing-acme.service healthy after restart" "CH11: affected tenant recovered"
assert_eq "$(restarts_of "$c" "lunarwing-acme.service")"   "1"  "CH11: affected tenant restarted"
assert_eq "$(restarts_of "$c" "lunarwing-globex.service")" "0"  "CH11: healthy tenant NOT restarted"
assert_eq "$(svc_state "$c" "lunarwing-globex.service")"   "up" "CH11: healthy tenant left untouched"

echo "=== CH14: restart 'succeeds' but unit crash-loops (SubState=auto-restart) → verify rejects (F5) ==="
c="$(sb)"; mk_mockbin "$c"
jq -n --arg u "$me" '{tenants:{acme:{user:$u}}}' > "$c/ports.json"
svc_down "$c" "lunarwing-acme.service"
svc_substate "$c" "lunarwing-acme.service" auto-restart   # is-active will read 'up' post-restart, but it's flapping
report "$c" '{components:[{component:"systemd",status:"critical",metrics:{units:[
  {name:"lunarwing-acme.service",status:"critical"}]}}]}'
o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false SELF_HEAL_TENANTS_FILE="$c/ports.json")"
assert_contains "$o" "not stable after restart (substate=auto-restart)" "CH14: verify rejects crash-looping unit"
assert_ne "$(restarts_of "$c" "lunarwing-acme.service")" "0"  "CH14: a restart was attempted"
assert_ne "$(state_of "$c")" "{}"                            "CH14: crash-looping unit kept in retry state (not cleared)"

echo "=== CH15: fail/recover cycles retain flap history and eventually escalate ==="
c="$(sb)"; mk_mockbin "$c"; report "$c" "$GW"
last=""
for _ in 1 2; do
    svc_down "$c" lunarwing
    run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_FLAP_MAX_RESTARTS=2 >/dev/null
    report "$c" "$HEALTHY_GW"
    run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_FLAP_MAX_RESTARTS=2 >/dev/null
    report "$c" "$GW"
done
svc_down "$c" lunarwing
last="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_FLAP_MAX_RESTARTS=2)"
assert_contains "$last" "FLAPPING: lunarwing" "CH15: recovery cycles do not erase flap history"
assert_eq "$(restarts_of "$c" lunarwing)" "2" "CH15: flap threshold prevents third restart"
assert_eq "$(state_of "$c" '.lunarwing.escalated')" "true" "CH15: repeated recovered flaps escalate"

echo "=== CH16: wedged tenant user bus -> bounded, counted as failed restart ==="
# A hung `systemctl --user` (dead tenant session bus) must not stall the whole
# self-heal tick: SELF_HEAL_SYSTEMD_PROBE_TIMEOUT bounds each tenant-bus call.
if command -v timeout >/dev/null 2>&1; then
    c="$(sb)"; mk_mockbin "$c"
    jq -n --arg u "$me" '{tenants:{acme:{user:$u}}}' > "$c/ports.json"
    svc_down "$c" "lunarwing-acme.service"
    report "$c" '{components:[{component:"systemd",status:"critical",metrics:{units:[
      {name:"lunarwing-acme.service",status:"critical"}]}}]}'
    # Mock a wedged tenant bus: every sudo-into-user-manager call hangs.
    cat > "$c/bin/sudo" <<'EOF'
#!/usr/bin/env bash
sleep 300
EOF
    chmod +x "$c/bin/sudo"
    start=$SECONDS
    o="$(run_chaos "$c" systemd SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false \
        SELF_HEAL_TENANTS_FILE="$c/ports.json" SELF_HEAL_SYSTEMD_PROBE_TIMEOUT=2)"
    elapsed=$((SECONDS - start))
    assert_contains "$o" "RESTART_FAIL" "CH16: wedged tenant bus counted as failed restart"
    assert_contains "$o" "Self-Healing complete" "CH16: tick completes despite the wedged bus"
    assert_range "$elapsed" 0 30 "CH16: tick bounded (~2s probe timeout, not 300s hang)"
    assert_eq "$(state_of "$c" '.["lunarwing-acme.service"].retries // 0')" "1" "CH16: retry recorded for next tick"
else
    ok "CH16: SKIP (timeout unavailable)"
    ok "CH16: SKIP (timeout unavailable)"
    ok "CH16: SKIP (timeout unavailable)"
    ok "CH16: SKIP (timeout unavailable)"
fi

echo "=== CH17: wedged OpenRC rc-service -> bounded, counted as failed restart ==="
# A hung rc-service (wedged supervise-daemon) must not stall the whole self-heal
# tick: SELF_HEAL_PROBE_TIMEOUT bounds every OpenRC status/restart call.
if command -v timeout >/dev/null 2>&1; then
    c="$(sb)"; mk_mockbin "$c"
    svc_down "$c" "lunarwing-acme"
    report "$c" '{components:[{component:"openrc",status:"critical",metrics:{services:[
      {name:"lunarwing-acme",status:"critical"}]}}]}'
    # Mock wedged OpenRC: every rc-service call hangs.
    cat > "$c/bin/rc-service" <<'EOF'
#!/usr/bin/env bash
sleep 300
EOF
    chmod +x "$c/bin/rc-service"
    start=$SECONDS
    o="$(run_chaos "$c" openrc SELF_HEAL_GRACE_CHECKS=1 SELF_HEAL_VERIFY_HEALTH=false \
        SELF_HEAL_PROBE_TIMEOUT=2)"
    elapsed=$((SECONDS - start))
    assert_contains "$o" "RESTART_FAIL" "CH17: wedged rc-service counted as failed restart"
    assert_contains "$o" "Self-Healing complete" "CH17: tick completes despite the wedged rc-service"
    assert_range "$elapsed" 0 30 "CH17: tick bounded (~2s probe timeout, not 300s hang)"
    assert_eq "$(state_of "$c" '.["lunarwing-acme"].retries // 0')" "1" "CH17: retry recorded for next tick"
else
    ok "CH17: SKIP (timeout unavailable)"
    ok "CH17: SKIP (timeout unavailable)"
    ok "CH17: SKIP (timeout unavailable)"
    ok "CH17: SKIP (timeout unavailable)"
fi

echo
echo "Note: CH7 (DNS failure) is modeled by CH6's stuck-service escalation path."
echo "Note: CH8 (disk full) requires real disk-fault injection and is out of scope"
echo "      for the mock harness — exercise it on the integration test machine."

finish
