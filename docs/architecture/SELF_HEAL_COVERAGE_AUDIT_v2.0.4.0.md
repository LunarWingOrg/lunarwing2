# Self-Heal Coverage Audit v2.0.4.0

**Date:** 2026-08-03 (ported and hardened on v2.0.5.0 on 2026-08-05; content re-verified, 435/435 assertions green)
**Status:** Goal 2 implementation complete; residual gaps documented below
**Reference reviewed:** `origin/aj/kimi-24-20260801-1203`
**Ported from:** `origin/shexpansion/cmc/v2.0.4.0` (commit `d1d3c52`)

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
| OpenRC | Init-script discovery, status, and persisted worker intent | `rc-service` through shared remediation path; intentionally stopped workers skipped | OpenRC, worker lifecycle, matrix, chaos mocks |
| launchd | Agent report | Existing stop/start path | matrix |
| Escalation | Max retries, flapping, monitor-only failures | Timeout, custom notifier, confirmed-delivery cooldown | matrix |

## Hardening Added

- Per-service cooldown keys use readable sanitized prefix plus SHA-256 digest.
- Report-supplied service names cannot escape state directory through escalation
  report filename.
- Missing `GOTIFY_TOKEN`, notifier failure, or timeout does not arm cooldown.
- Undelivered or cooldown-deferred escalation remains retryable; only confirmed
  delivery sets `escalated:true`.
- Dry-run preserves `state.json` and corrupt-state evidence.
- Recovery clears retries and streaks but retains in-window restart history.
- `lunarwing-vision-<tenant>` resolves to correct tenant before generic prefix.
- Active-but-unhealthy LunarVision containers become critical in systemd report.
- OpenRC Vision units resolve to their real tenant before started-state gating.
- OpenRC worker stop/kill intent survives `start-tenant` and reboot; stale direct
  and babysitter runlevel links are removed, while health skips intentionally
  stopped or unselected workers.
- OpenRC worker babysitter names resolve to their real tenant, keeping crashes
  critical when worker remains selected and desired-running.
- Host-global MT config pins base service scope to system bus.
- Single-node user-timer probes and remediation share the configured user bus.
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

> **Update (2026-08-13, v2.0.7.0):** `run-all.sh` has since grown beyond the
> seven suites listed above. It now also registers `init-templates`
> (LunarVision init templates), `lunarvoice`, and `voice-init-templates` —
> LunarVoice health coverage (`health-lunarvoice.sh`) did not exist at audit
> time. LunarVision and LunarVoice tenant lifecycles were further hardened in
> v2.0.7.0 (distinct registry health ports, `-sup` crash supervisors on
> OpenRC, health-gated starts, drifted-container recreation); the audit's
> coverage table otherwise still reflects the pipeline's structure.
