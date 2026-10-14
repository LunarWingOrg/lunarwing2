# Self-Heal Coverage Audit v2.0.4.0

**Date:** 2026-08-03 (ported and hardened on v2.0.5.0 on 2026-08-05; 435/435 assertions were green at that audit point; suite registration re-verified 2026-09-28, again 2026-10-01 for v2.1.4.0, again 2026-10-06 for v2.1.5.0, and again 2026-10-14 for v2.1.6.0)
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

> **Update (2026-08-19, v2.0.8.0):** `metrics-exporter`
> (`test-health-metrics-exporter.sh`) became the eleventh registered suite for
> the healthpoint metrics exporter landed in that cycle.
> `health-metrics-exporter.sh` runs from `cron-wrapper.sh` after each check
> and flattens the newest aggregated report plus self-heal `state.json` into
> Prometheus textfile exposition (per-check `duration_seconds` is stamped by
> `infrastructure-health-check.sh`). A graduated `THRESHOLD_<metric>`
> early-warning layer with a deduplicated Gotify path and an optional
> loopback-only listener crate (`ic/crates/lunarwing_health_metrics`,
> installed by `ic/scripts/install-health-metrics-listener.sh` behind the
> ports-registry v16 `health_metrics` slot) accompany it. Design:
> `docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`.
>
> **Update (2026-08-25, v2.0.9.0):** the canonical aggregate now registers 14
> suites. In addition to the eleven above, `metrics-unit` validates the
> loopback listener's hardened systemd unit, `watchdog` covers the systemd
> service watchdog (including user-manager routing), and `watchdog-openrc`
> covers the OpenRC wrapper and bounded `rc-service` probes. The ordered,
> authoritative list is the `SUITES`/`ORDER` pair in `tests/run-all.sh`; use
> `bash tests/run-all.sh --list` rather than copying a suite count into tooling.
> The audit's detection/remedy table still describes the self-heal pipeline
> itself.
>
> **Update (2026-09-11, v2.1.1.0):** the `ORDER` list is unchanged at 14
> suites; `llm-streams` (registered since v2.0.9.0) covers the LLM stream
> integrity healthpoint (`health-llm-streams.sh`).
>
> **Update (2026-09-12, v2.1.2.0):** the `ORDER` list is still unchanged at
> 14 suites
> (`regression matrix chaos openrc systemd lunarvision init-templates
> lunarvoice voice-init-templates netbird llm-streams metrics-exporter
> metrics-unit watchdog watchdog-openrc`). No self-heal pipeline script
> changed in the 2.1.2.0 cycle; the writer-quiescence work (WI#133/#134/#137)
> lives in mt-admin tenant lifecycle, not the host health pipeline, and is
> documented in `SELF_HEAL_DEPLOYMENT_WIRING.md`.
>
> **Update (2026-09-19, v2.1.2.0):** the `ORDER` list grew from 14 to 20 suites
> after the WI#180 health-system-parity work. The six new registered suites are
> `cron-wrapper`, `runtime`, `knowledge`, and `gateway` (plus the already
> counted `metrics-exporter`/`metrics-unit` pair was already present; the new
> additions bring the total to 20). The canonical list is now
> (`regression matrix chaos openrc systemd lunarvision init-templates
> lunarvoice voice-init-templates netbird llm-streams metrics-exporter
> metrics-unit watchdog watchdog-openrc cron-wrapper runtime knowledge
> gateway`). The `runtime` and `knowledge` suites cover the WI#180
> tenant-aware runtime diagnostics probe and knowledge publication/sync/sweep
> monitoring; `gateway` covers the stale-lock and oversized-session gateway
> checks addressed in the MR !1041 review; `cron-wrapper` validates the
> scheduled launcher PATH hygiene (WI#180 F3). The audit's detection/remedy
> table still describes the self-heal pipeline itself.
>
> **Update (2026-09-28, v2.1.3.0):** the `ORDER` list is unchanged in the
> 2.1.3.0 cycle; `bash tests/run-all.sh --list` currently registers **19**
> suites. The 2026-09-19 entry above overcounted by one (it said 20): the
> canonical list it quotes is the same 19 suites
> (`regression matrix chaos openrc systemd lunarvision init-templates
> lunarvoice voice-init-templates netbird llm-streams metrics-exporter
> metrics-unit watchdog watchdog-openrc cron-wrapper runtime knowledge
> gateway`). No self-heal pipeline script changed in the 2.1.3.0 cycle; the
> release's tenant-env work (WI#239) lives in mt-admin env generation, not the
> host health pipeline.
>
> **Update (2026-10-01, v2.1.4.0):** the `ORDER` list is unchanged in the
> 2.1.4.0 cycle (`bash tests/run-all.sh --list` still registers the same 19
> suites, verified live). No self-heal pipeline script changed this cycle;
> the release's feature work (WI#191 tool-estimate hints, WI#220 prompt
> editor, WI#265 `routine_history`) lives in the agent/gateway, not the host
> health pipeline.
>
> **Update (2026-10-06, v2.1.5.0):** the `ORDER` list is unchanged in the
> 2.1.5.0 cycle (`bash tests/run-all.sh --list` still registers the same 19
> suites, verified live). One pipeline script changed this cycle:
> `health-llm-streams.sh` now also exports `retry_reason_empty_total` per
> provider (WI#297; empty-stream retries increment the telemetry
> `retry_reasons.empty` counter, and the metrics exporter picks the numeric
> field up dynamically). The audit's detection/remedy table still describes
> the self-heal pipeline itself.
>
> **Update (2026-10-14, v2.1.6.0):** the `ORDER` list is unchanged in the
> 2.1.6.0 cycle (`bash tests/run-all.sh --list` still registers the same 19
> suites, verified live). No self-heal pipeline script changed this cycle;
> the release's WI#376 mt-admin hardening (init detection, OpenRC stop/units,
> restore probe) lives in `ic/scripts/mt-admin-lib/`, not the host health
> pipeline, and is documented in `SELF_HEAL_DEPLOYMENT_WIRING.md`. The
> audit's detection/remedy table still describes the self-heal pipeline
> itself.

