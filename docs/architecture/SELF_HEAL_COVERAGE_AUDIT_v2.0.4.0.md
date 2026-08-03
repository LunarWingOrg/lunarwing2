# Self-Heal Coverage Audit v2.0.4.0

**Date:** 2026-08-03
**Status:** Goal 2 implementation complete; residual gaps documented below
**Reference reviewed:** `origin/aj/kimi-24-20260801-1203`

## Outcome

Current branch ports useful reference behavior without importing unsafe defaults.
NetBird remains operator-owned and disabled by default. Escalation cooldown is
per service, collision-resistant, and starts only after confirmed notification
delivery. Restart history survives healthy observations, preserving flap guard
across fail/recover cycles.

## Coverage

| Area | Detection | Remedy / policy | Test coverage |
|------|-----------|-----------------|---------------|
| Gateway, XMPP, TensorZero, ClickHouse | Logical checks plus init reports | Grace, restart, verify, backoff, flap escalation | Regression, matrix, chaos |
| LunarVision | Registry-aware HTTP check plus systemd container health | Single-node logical service or discovered tenant unit | LunarVision, systemd, matrix |
| NetBird | Opt-in `netbird status --check startup` | Escalation-only by default; restart requires explicit delegation | NetBird probe, matrix |
| systemd MT | Tenant registry plus user-manager enumeration | Tenant owner user bus with `XDG_RUNTIME_DIR` | systemd, matrix, chaos |
| systemd single-node | Base unit health | Configurable `system`/`user` scope | matrix |
| OpenRC | Init-script discovery and status | `rc-service` through shared remediation path | OpenRC, matrix, chaos mocks |
| launchd | Agent report | Existing stop/start path | matrix |
| Escalation | Max retries, flapping, monitor-only failures | Timeout, custom notifier, confirmed-delivery cooldown | matrix |

## Hardening Added

- Per-service cooldown keys use readable sanitized prefix plus SHA-256 digest.
- Report-supplied service names cannot escape state directory through escalation
  report filename.
- Missing `GOTIFY_TOKEN`, notifier failure, or timeout does not arm cooldown.
- Recovery clears retries and streaks but retains in-window restart history.
- `lunarwing-vision-<tenant>` resolves to correct tenant before generic prefix.
- Active-but-unhealthy LunarVision containers become critical in systemd report.
- Host-global MT config pins base service scope to system bus.
- Single-node user-timer instructions pin base service scope to user bus.
- Explicitly delegated NetBird daemon restart remains on host system bus.
- NetBird probe defaults off; enabled-but-missing CLI fails loud.

## Intentional Residual Gaps

1. Logical and init aliases (`lunarwing` vs `lunarwing.service`) remain distinct.
   Contradictory report entries can therefore target one alias while marking
   another healthy. Existing matrix case C6 documents behavior; normalization
   needs separate precedence design.
2. Tenant-looking unit absent from registry falls through to configured base
   systemd scope. Changing this to fail closed requires policy for stale or
   partially written registries.
3. Per-tenant application-level gateway/channel health remains deferred. Current
   MT coverage proves process, user-unit, container, and LunarVision endpoint
   health, not authenticated application channel health.
4. NetBird coverage uses mocked CLI. Live enrollment, management outage, and
   daemon restart delegation need operator-controlled staging validation.
5. OpenRC and launchd behavior is mock-tested here; live platform validation
   remains deployment responsibility.

## Verification Contract

`ic-infrastructure-health-check/tests/run-all.sh` is canonical aggregate suite.
It includes regression, matrix, chaos, OpenRC, systemd, LunarVision, and NetBird
suites. CI invokes aggregate once through `.gitlab/scripts/shell-harnesses.sh`.
