# Self-Healing Coverage Audit — v2.0.4.0

**Date:** 2026-08-01 (branch `aj/kimi-24-20260801-1203`, goals item #24)
**Status:** Reference — audit of what the health → self-heal pipeline covers
after the v2.0.3.0 tunnel / MCP / worker / sidecar changes, what was
implemented to close the gaps, and what remains deferred.
**Related:** `docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md`,
`docs/proposals/IC_REPAIR_FOLLOWUPS.md`,
`docs/proposals/CHAOS_FOLLOWUP_TESTS.md`,
`ic-infrastructure-health-check/README.md`

The v2.0.6 roadmap books "Self-Healing Capabilities analysis of any missing
pieces from all the new components. Implementation of missing pieces to
follow" for v2.0.4. This document is the analysis; the implementations noted
below landed on the same branch.

## Method

Every deployable component was checked against two questions:

1. **Detection** — does a health check observe it (directly, or via the
   init-system unit scans `health-systemd.sh` / `health-openrc.sh` /
   `health-launchd.sh`)?
2. **Remediation** — can `lunarwing-self-heal.sh` restart it (and verify
   recovery), or is escalation the designed terminal action?

## Component inventory

| Component | Detection | Remediation | Notes |
|---|---|---|---|
| `lunarwing-<t>` daemon | init unit scan (both inits) + `health-gateway.sh` (session files) | restart via init; per-tenant systemd user units resolved through `ports.json` | app-level channel health still deferred (see below) |
| `xmpp-bridge-<t>` | init unit scan + `health-xmpp.sh` reachability | restart via init | |
| `lunarwing-proxy-<t>` (TensorZero proxy) | init unit scan + `health-tensorzero.sh` | restart via init | legacy `ironclaw-proxy-<t>` names still matched |
| `lunarwing-pg-<t>` (per-tenant PostgreSQL) | init unit scan | restart via init | |
| External workers (`lunarwing-{nanocode,opencode,kimi,pebble}-<t>`) | init unit scan (systemd glob `lunarwing-*`, OpenRC `/etc/init.d/lunarwing-*`) | restart via init | covered since the worker units were added; verified in `unit_tenant()` prefix table |
| WeeChat (`lunarwing-weechat-<t>`, `-adapter-<t>`) | init unit scan + per-tenant `enable_weechat` health gate (v2.0.4.0 item #3) | restart via init; `skipped` (never critical) when disabled for the tenant | flap/`render-units` footgun closed in item #3 |
| DarkIRC (`lunarwing-darkirc-<t>`, `-adapter-<t>`) | init unit scan (`lunarwing-*` glob) | restart via init | |
| LunarVision OCR/VL sidecar | `health-lunarvision.sh` (per-tenant `vision_health` registry ports) + container-unit scan | `lunarvision → ocr-sidecar` (`LUNARVISION_SERVICE`); container units via init scan | |
| **NetBird tunnel daemon** (new in v2.0.3.0) | **`health-netbird.sh` — ADDED this audit** | **`netbird → netbird` (`NETBIRD_SERVICE`) — ADDED; host-global, exempt from the MT `REMEDY_LOGICAL=false` skip** | was the largest blind spot: operator-managed daemon, so its death silently killed every tenant tunnel |
| Host-local MCP servers | none (in-daemon child processes) | daemon restart (`lunarwing-<t>`) is the remedy | MCP lifecycle is owned by the agent process; no host-level unit to watch |
| ClickHouse | `health-clickhouse.sh` | `clickhouse → clickhouse-server` | |
| OMEMO store / rate-limit state / external model APIs | `health-{omemo,ratelimit,models}.sh` | **none by design** (`NO_REMEDY`) — feature stores and external SaaS have nothing to restart | detection only; appear in reports/alerts |
| Gotify (pager itself) | indirect — notifier failures logged, curl bounded | none | if Gotify is down the escalation can't page; the failure is logged in `actions.log`. A dead notifier no longer arms the page cooldown (see below) |

## Mechanism gaps closed in this audit (v2.0.4.0 item #24)

1. **Escalation page flood (IC_REPAIR_FOLLOWUPS 6a — was the last open §A
   item).** A flap→recover→reflap loop re-paged Gotify on every cycle because
   recovery erases the state entry. Added `SELF_HEAL_ESCALATE_COOLDOWN`
   (default 3600 s, `0` disables): a per-service sidecar epoch
   (`.last_escalation_page.<svc>`) gates repeat pages. Per-service — not
   global — so one noisy service cannot suppress a different service's first
   page; written only after the notifier exits 0, so a failed page never arms
   the cooldown; kept outside `state.json` so `clear_service_state` /
   `prune_state` can't erase it (that erase is exactly the flap cycle).
   Suppressed pages still write the escalation report and keep
   `escalated=true`. Tests: chaos CH15a–d, matrix section P.

2. **NetBird tunnel daemon blind spot.** New `health-netbird.sh`: bounded
   `netbird status` probe; daemon unresponsive → critical, management plane
   disconnected → degraded. Auto-disables (healthy, `enabled=false`) when the
   binary is absent, so `none`/`custom` tunnel deployments see no false
   criticals. Wired into the orchestrator and the self-heal service map with
   post-restart verification. Because the daemon is host-global (one daemon
   carries every tenant's `netbird expose`), the component is exempt from the
   MT-mode `REMEDY_LOGICAL=false` skip. Tests: `test-health-netbird.sh`
   (28 assertions), matrix B10–B12.

3. **Test-suite registration gap.** `run-all.sh` now registers the
   `lunarvision` suite (existed but was never run by the aggregate runner)
   and the new `netbird` suite. Full run: **419 assertions, 7 suites, all
   green.**

## Still deferred (with reasons)

- **Per-tenant application-level health (Pattern C,
  `IC_REPAIR_FOLLOWUPS.md` §10).** Deep-checking each tenant's authed
  `/api/gateway/status` `.channel_health` and mapping dead channels to the
  right per-tenant unit. Deferred by that document's own recommendation: it
  needs a dedicated design pass (liveness-vs-app conflict with
  report-as-truth recovery, channel→service remedy map, per-tenant token
  handling, `None`-channel absence semantics, N-tenant probe budget).
- **CHAOS_FOLLOWUP_TESTS sections O–S (partial).** Concurrency races beyond
  the flock test, more malformed-state shapes, report clock-skew handling,
  flap boundary precision. P1 (truncated state) landed as matrix section O;
  the rest are test-only hardening, not capability gaps.
- **Gotify self-monitoring.** The pager has no health check of its own. The
  practical mitigation landed: notifier failures are logged and no longer arm
  the escalation cooldown, so a recovered Gotify doesn't find a backlog of
  suppressed pages. A dedicated `health-gotify.sh` would only help when
  Gotify runs host-local; revisit if that becomes the standard deployment.

## Verification

- `bash ic-infrastructure-health-check/tests/run-all.sh` → 7 suites,
  419 passed / 0 failed (regression 28, matrix 137, chaos 53, openrc 43,
  systemd 49, lunarvision 81, netbird 28).
- Live host probe: real `netbird` 0.75.0 daemon reported
  healthy/connected; MT-mode dry-run confirmed per-tenant logical components
  skip while host-global `netbird` remediates.
