# Proposed Follow-up Tests for Self-Healing Test Suite

> **Current status (reverified 2026-08-25): PARTIAL, WITH MOST P0/P4
> MECHANISMS AND SEVERAL BOUNDARIES LANDED.** Current coverage includes lock
> contention (proposal O1/O2 equivalent), truncated/empty state quarantine (P1
> plus the zero-byte case), stale restart-history filtering (P3), missing and
> failing notifier behavior (R1/R3), collision-safe per-service cooldown (R4),
> notifier timeout (R5), threshold/below-threshold/stale-window flap cases
> (S1–S3 equivalents), history retention across recovery, and bounded wedged
> systemd/OpenRC probes. `SELF_HEAL_MAX_REPORT_AGE` also implements the Q1
> feature, but it still lacks a dedicated test here. SIGTERM lock release,
> malformed typed state, future/mixed history, clock skew, schema versioning,
> and the remaining report/flap boundary cases are still open. Proposal IDs
> below are historical and do not match the suite's later section lettering.

*Drafted after P1/P2/P3 review items landed (commit `9c0c16ff` on
`2026-06-14-chaos-baud`).*

The maintained aggregate is
`ic-infrastructure-health-check/tests/run-all.sh` (14 registered suites as of
this audit). `test-self-heal-matrix.sh` now includes lock, escalation cooldown,
flap, recovery, and truncated-state sections; `chaos-harness.sh` extends through
CH17. The tables below are the remaining-coverage inventory, not a current test
count.

---

## Section O — Concurrency Safety

| ID | Scenario | Fault Inject | Current contract / status |
|----|----------|-------------|---------------------------|
| O1 | Two simultaneous runs | Two instances racing on same `state.json` | **Covered in equivalent form:** a rival-held `flock` makes the second run warn and exit without work |
| O2 | Nested/concurrent `sudo` calls | Second instance starts while first holds flock | **Covered by the same lock regression:** `WARNING: another self-heal instance is running; exiting` |
| O3 | SIGTERM during locked section | Kill mid-execution while holding lock | **Open:** verify process-exit lock release and a clean next run |

## Section P — State Corruption Recovery

| ID | Scenario | Fault Inject | Current contract / status |
|----|----------|-------------|---------------------------|
| P1 | Truncated `state.json` | `echo -n "{lunarwing":' > state.json` (incomplete JSON) | **Covered:** fixed-name `.corrupt` quarantine, fresh state, bounded repeated corruption; zero-byte and dry-run cases also covered |
| P2 | Valid-but-malformed jq structure | `{"lunarwing":{"consecutive_unhealthy":"seven"}}` | **Open:** typed field validation/coercion is not covered |
| P3 | Stale `restart_history` timestamps | History contains old/future epochs | **Partial:** old entries outside the window are covered; future timestamps still need a regression |
| P4 | Mixed types in `restart_history` | History contains strings like `"abc"` instead of epochs | **Open:** skip/reset behavior must be specified and tested |
| P5 | State file written by another agent | Different schema version inside state.json | **Open:** state has no schema-version migration contract |

## Section Q — Report Staleness & Time Handling

| ID | Scenario | Fault Inject | Current contract / status |
|----|----------|-------------|---------------------------|
| Q1 | Report mtime exceeds configured age | Backdate `report.json` and set `SELF_HEAL_MAX_REPORT_AGE` | **Feature landed, test open:** configured positive limit refuses stale remediation; default `0` deliberately disables the guard |
| Q2 | Clock skew / future report | Report has future mtime/timestamp | **Open:** define and test future-mtime handling |
| Q3 | Report missing required component keys | `{"status":"degraded"}` without `component`, `metrics` | **Open regression:** should skip malformed entries without unsafe remediation |
| Q4 | Report has unknown top-level keys | `{"components":[],"unknown_key":null}` | **Open regression:** extras should remain ignored |
| Q5 | Empty `components` array | `{"components":[]}` | **Open regression:** no-op and clean exit |

## Section R — Escalation Path Edge Cases

| ID | Scenario | Fault Inject | Current contract / status |
|----|----------|-------------|---------------------------|
| R1 | `send-notification.sh` missing | No script on disk | **Covered:** warns, completes, and leaves escalation unlatched so delivery retries later |
| R2 | Escalation script non-executable | Script exists but `-x` not set | **Open direct regression;** current delivery gate treats it as unavailable and must leave escalation retryable |
| R3 | Escalation script exits non-zero | Script exits 1 | **Covered:** warns, retries next tick, and sets `escalated=true` only after confirmed delivery |
| R4 | Escalation rate-limited | Repeated confirmed delivery | **Covered:** collision-resistant per-service sidecars suppress within `SELF_HEAL_ESCALATE_COOLDOWN`; failed/missing-token delivery never arms cooldown |
| R5 | Escalation script blocks/hangs | Script sleeps 60s | **Mechanism landed:** `SELF_HEAL_ESCALATE_TIMEOUT` (default 30s); a dedicated sleeping-notifier assertion remains useful |

## Section S — Flap Detection Boundary Cases

| ID | Scenario | Fault Inject | Current contract / status |
|----|----------|-------------|---------------------------|
| S1 | Exactly `FLAP_MAX_RESTARTS` | 5 in-window restarts with threshold 5 | **Covered and corrected:** escalates at `>=`, so exactly the threshold is terminal |
| S2 | Below threshold / threshold+1 | 4 or 6 in-window restarts with threshold 5 | **Partial:** below-threshold restart and exactly-threshold escalation are covered; threshold+1 is implied but not separate |
| S3 | Restarts spanning multiple windows | Old and current epochs | **Partial:** entirely stale history is covered; a mixed two-window fixture remains open |
| S4 | Very long flap window | `FLAP_WINDOW_SECS=86400` (24h) | **Open** |
| S5 | `FLAP_MAX_RESTARTS=0` | Any unhealthy observation | **Open:** validate the degenerate setting before promising behavior |

---

## Implementation Notes

- All tests fit the existing sandbox framework (`sb()`, `run_dry()`, `run_chaos()`)
- **O** tests need a background `flock` holder + race timing
- **P** tests need explicit malformed `state.json` seeding before invocation
- **Q** tests need `touch -d` tricks (BSD vs GNU portability matters)
- **R** tests need fake `send-notification.sh` that fails, hangs, or counts invocations
- **S** tests need time-skewed `restart_history` entries and explicit `FLAP_*` env

---

## Remaining priorities

| Priority | Items | Rationale |
|----------|-------|-----------|
| **P0** | O3, P2, P4, Q2, Q3 | Process-exit and malformed-input behavior can otherwise fail a whole tick |
| **P1** | Q1 test, Q4, Q5, R2, R5 test | Pin already-intended fail-safe behavior and landed feature boundaries |
| **P2** | P3 future/mixed case, S2 +1, S3 mixed windows, S4, S5 | Boundary precision and degenerate configuration |
| **P3** | P5 | A schema/version contract is useful before state shape changes again |

---

## Self-Heal Improvements Surfaced by These Tests

1. **Staleness check (Q1) — implemented:** `SELF_HEAL_MAX_REPORT_AGE` refuses
   remediation when the selected report's mtime exceeds a configured positive
   limit; `0` disables the guard. Add the missing regression.
2. **State schema version (P5) — open:** add `_meta.version` only with explicit
   migration/reset compatibility behavior.
3. **Escalation rate limit (R4) — implemented:**
   `SELF_HEAL_ESCALATE_COOLDOWN` uses per-service digest-keyed sidecars and starts
   only after confirmed delivery.
4. **Escalation script timeout (R5) — implemented:**
   `SELF_HEAL_ESCALATE_TIMEOUT` defaults to 30 seconds and uses GNU `timeout`
   when available; add a direct sleeping-notifier regression.

---

— Baud 🦄
