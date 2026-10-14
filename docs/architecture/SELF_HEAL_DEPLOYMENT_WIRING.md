# Infrastructure Self-Healing — Deployment & Provisioning Wiring

**Date:** 2026-06-13 (revalidated 2026-09-28, v2.1.3.0; WI#180 health parity — opencode2 container-health role, monitor-only runtime/knowledge diagnostics; revalidated 2026-10-01, v2.1.4.0 — pipeline wiring, scheduling ownership, and role mappings unchanged this cycle; revalidated 2026-10-06, v2.1.5.0 — `ensure_health_pipeline()` still lives in `mt-admin-lib/12-health-pipeline.sh` and is still invoked from `add_tenant()`; writer-quiescence stop path and watchdog installer ownership unchanged; the only pipeline-script change this cycle is the `health-llm-streams.sh` `retry_reason_empty_total` export (WI#297), which does not touch this wiring; revalidated 2026-10-14, v2.1.6.0 — no change under `ic-infrastructure-health-check/` in the cycle; `ensure_health_pipeline()` still at `mt-admin-lib/12-health-pipeline.sh` and still invoked from `add_tenant()` in `mt-admin-lib/04-tenant-lifecycle.sh`; the WI#376 mt-admin hardening (FL-1 init-detection preflight, FL-2 restore probe rc+state classification, FL-3 symlinked/non-regular OpenRC units never "already stopped") lives in `mt-admin-lib/08-units-openrc.sh` (FL-1 at the OpenRC dispatch preflight, FL-3 in the stop helper) and `mt-admin-lib/11-postgres.sh` (FL-2 in the restore probe) — not in the modules this doc's pipeline sections name — and does not change the pipeline structure, scheduling ownership, or the two-watchdog split; `ic/scripts/enable-health-fleet.sh` (the standalone fleet enabler that calls `ensure_health_pipeline` itself) additionally gained a WI#376 EQ-3 refusal gate: beside a legacy OpenRC proxy unit it refuses with the `render-units --all-proxies` remedy instead of provisioning; the WI#180 suite-count sentence is corrected to 19, matching the audit's canonical correction)
**Status:** Reference (as-is) — documents current behavior, not a proposal
**Related:** `ic-infrastructure-health-check/README.md`, `docs/ops/MULTITENANCY-PRODUCTION.md`, `docs/internal/history/proposals/CHAOS_ENGINEERING_TEST_PLAN.md`

## Summary

The infrastructure health-check + self-heal pipeline (`ic-infrastructure-health-check/`)
is a **host-level** facility that covers all tenants automatically (registry /
init-scan discovery). As of v1.1.9, `lunarwing-mt-admin.sh add-tenant` calls
`ensure_health_pipeline()` which installs the pipeline scripts, writes a config
env file, and schedules the pipeline (systemd timer or OpenRC cron) in one step.
It can be opted out with `--no-health`.

> **v2.0.8.0 note (mt-admin refactor):** `ic/scripts/lunarwing-mt-admin.sh` is
> now a thin composition root that sources `ic/scripts/mt-admin-lib/NN-*.sh`
> modules. `ensure_health_pipeline()` lives in
> `mt-admin-lib/12-health-pipeline.sh` and is invoked from `add_tenant()` in
> `mt-admin-lib/04-tenant-lifecycle.sh`; the CLI surface described here is
> unchanged.

The watchdog installer (`install-lunarwing-watchdog.sh`) is a separate,
manually-run host-level step focused on the **service-level watchdog** (restarts
the base LunarWing daemon if down). On systemd and OpenRC, it also copies the
self-heal scripts into `/usr/local/sbin` but does **not** schedule them — that
scheduling is handled by the `add-tenant` `ensure_health_pipeline()` flow.
The launchd installer installs only its user-level service watchdog wrapper and
LaunchAgent; mt-admin does not install the host-global self-heal pipeline on
launchd.

> **Scheduling ownership:** the self-heal pipeline is scheduled exclusively by
> `add-tenant` / `ensure_health_pipeline()`. The `install-lunarwing-watchdog.sh`
> installer copies the self-heal scripts to `/usr/local/sbin` on systemd and
> OpenRC but never schedules them. On a host where the watchdog installer ran
> but `add-tenant` has not, the scripts are on disk but unscheduled.

## Two distinct "watchdogs" (don't conflate them)

| | Service-level watchdog | Infrastructure self-heal |
|---|---|---|
| Code | `ic/scripts/lunarwing-watchdog.sh`, `lunarwing-watchdog-openrc.sh`, `lunarwing-watchdog-launchd.sh` | `ic-infrastructure-health-check/lunarwing-self-heal.sh` |
| Scope | Restarts the base daemon if down | Reads health-check reports, remediates any unhealthy component/unit (including per-tenant units), with grace / backoff / flap-guard / escalation |
| Scheduled by | Installer-created systemd timer, OpenRC cron/fcron hook, or launchd user LaunchAgent | `lunarwing-mt-health.timer` / managed cron (scheduled by `add-tenant` via `ensure_health_pipeline()`) |
| Unit | `ic/systemd/lunarwing-watchdog.{service,timer}`, OpenRC scheduler hook, or `ic/systemd/com.lunarwing.watchdog.plist` | `lunarwing-mt-health.{service,timer}` (systemd) or managed fcron/crontab entry (OpenRC) |

The self-heal pipeline is the subject of the chaos test suite in
`ic-infrastructure-health-check/tests/`.

## The health → self-heal pipeline

`cron-wrapper.sh` is the entry point that runs the full loop:

```
cron-wrapper.sh
  ├── infrastructure-health-check.sh   → writes JSON report to
  │       $LUNARWING_BASE_DIR/workspace/reports/health/<ts>.json
  │       (runs health-{gateway,xmpp,…}.sh + health-{systemd,openrc,launchd}.sh)
  ├── health-metrics-exporter.sh       → flattens the fresh report + self-heal
  │       state.json into Prometheus textfile exposition (optional;
  │       HEALTH_METRICS_EXPORT_ENABLED=false skips it, failures are non-fatal)
  └── lunarwing-self-heal.sh            → reads the latest report, remediates
```

Health-check failures don't abort the run — `cron-wrapper.sh` always proceeds to
self-heal so a partial report can still drive remediation.

NetBird readiness is opt-in (`HEALTH_NETBIRD_ENABLED=true`) because enrollment
and daemon lifecycle are operator-owned. Failure escalates without daemon
restart unless operator also sets `SELF_HEAL_NETBIRD_REMEDIATE=true`.

## How it actually gets onto a host

There are two installation paths:

### Path 1: `mt-admin.sh add-tenant` (primary, automatic)

`lunarwing-mt-admin.sh add-tenant <name>` calls `ensure_health_pipeline()` as part
of tenant provisioning (unless `--no-health` is passed). This function:

1. Copies the pipeline scripts from `ic-infrastructure-health-check/` to a stable
   lib dir (`$HEALTH_LIB_DIR`) that survives repo/worktree moves.
2. Creates the host-level report/state dir.
3. Writes a config env file (`/etc/lunarwing/health.env`, mode 0600) if absent —
   includes `LUNARWING_BASE_DIR`, `LUNARWING_SERVICE_MANAGER`, tenant registry
   path, `SELF_HEAL_SYSTEMD_SCOPE=system`, disabled-by-default NetBird probing,
   and MT-hardening flags. If the file already exists, only the
   `LUNARWING_SERVICE_MANAGER` and `SELF_HEAL_TENANTS_FILE` lines are
   reconciled (preserving operator edits).
4. Writes a launcher script that sources the env file and execs `cron-wrapper.sh`.
5. Schedules the launcher:
   - **systemd**: installs + enables `lunarwing-mt-health.{service,timer}`
     (default: every 15 min). The oneshot is explicitly bounded with
     `TimeoutStartSec=max(900s, interval)` — the ~90s distro default can kill a
     legitimate multi-tenant remediation mid-restart, and a wedged tenant user
     bus must never stall the pipeline forever (a killed run is replayed via
     `Persistent=true`). Per-tenant user-bus calls inside self-heal are
     individually bounded too (`SELF_HEAL_SYSTEMD_PROBE_TIMEOUT`, default 15s).
   - **OpenRC**: installs a managed `fcron`/`crontab` entry every 5 min
     (managed block).

   Set `LUNARWING_MT_HEALTH_INTERVAL_MIN` to override either default.

Because the pipeline is host-global and auto-discovers tenants, the scheduling
is idempotent — subsequent `add-tenant` calls re-run `ensure_health_pipeline()`
without duplicating the schedule.

### Path 2: `install-lunarwing-watchdog.sh` (manual, complementary)

`ic/scripts/install-lunarwing-watchdog.sh` (run as root on systemd/OpenRC;
launchd runs rootless as a user agent; run once per host) auto-detects all three
service managers and installs the **service-level watchdog**:

- **systemd:** installs and enables `lunarwing-watchdog.{service,timer}` plus
  `/usr/local/sbin/lunarwing-watchdog`.
- **OpenRC:** installs `/usr/local/sbin/lunarwing-watchdog-openrc`, preserves or
  creates `/etc/conf.d/lunarwing-watchdog`, smoke-runs the wrapper, and chooses
  an existing hourly cron path or a managed fcron entry.
- **launchd:** installs a user wrapper and
  `~/Library/LaunchAgents/com.lunarwing.watchdog.plist`; it runs at load and
  hourly at minute zero.

The systemd and OpenRC paths also conditionally copy
`lunarwing-self-heal.sh` to `/usr/local/sbin/lunarwing-self-heal` and
`cron-wrapper.sh` to `/usr/local/sbin/lunarwing-health-cron`. The launchd path
does not copy these pipeline scripts. On systemd, the watchdog probes the
system bus by default; for a single-node user-manager service, set
`LUNARWING_WATCHDOG_SYSTEMD_SCOPE=user` so every probe/restart uses
`systemctl --user` (invalid values warn and fall back to `system`).

The installer never schedules `lunarwing-health-cron`; Path 1 owns that schedule.
On a systemd/OpenRC host where `add-tenant` has not run, scheduling
`cron-wrapper.sh` remains a manual step documented in
`ic-infrastructure-health-check/README.md`.

## Multi-tenant provisioning now installs it

`lunarwing-mt-admin.sh add-tenant <name>` calls `ensure_health_pipeline()` as
the final step of tenant provisioning (unless `--no-health` is passed). The full
`add_tenant()` flow (`mt-admin-lib/04-tenant-lifecycle.sh`):

1. Allocate a port block (registry)
2. Create the tenant OS user
3. Clone the tenant repo
4. Write env/config files (vision / voice / LunarWing / bridge / optional proxy
   and DarkIRC / Gotify / external workers)
5. Bootstrap the WeeChat relay, provision the SSH harness, optionally sync and
   wire the Study Notes MCP (`--with-study-mcp`), and build the vision sidecar
   image if it is missing. A successful new-tenant Study Notes setup also appends
   `KNOWLEDGE_SEED_FROM_STUDY_MCP=1`; first daemon startup then imports the synced
   notes once without overwriting existing paths. `configure-study-mcp` and
   `upgrade-tenant` do not arm that seed. `--with-study-mcp` additionally
   enables the additive knowledge refresh cadence
   (`KNOWLEDGE_SYNC_ENABLED=true` plus its cadence/bound defaults) and installs
   the root-owned source scheduler (mt-admin module `14-knowledge-sync.sh`) so
   the published checkout keeps advancing; `configure-study-mcp
   --enable-knowledge-sync` / `--disable-knowledge-sync` toggle an existing
   tenant's cadence.
6. Start the per-tenant PostgreSQL container
7. Render the per-tenant init units (`lunarwing-<name>`, `xmpp-bridge-<name>`,
   `lunarwing-proxy-<name>`, weechat adapter, …)
8. On non-systemd inits, set up the pre-delegated cgroup v2 subtree (warn-only;
   repairable later via `setup-cgroups`)
9. **Call `ensure_health_pipeline()`** — installs + schedules the host-global
   health-check → self-heal pipeline (covers all tenants automatically).

### Tenant stop now verifies writer quiescence (2.1.2.0, WI#133/#134/#137)

`stop_tenant` and `stop_tenant_writers` (`mt-admin-lib/04-tenant-lifecycle.sh`)
used to stop every writer service/container without verifying the result. On
OpenRC, `supervise-daemon` teardown is asynchronous and a container with a
restart policy or a still-registered `-sup` babysitter can be respawned right
after the stop calls — so a tenant could end up with live writers while its
daemon and PostgreSQL are down. Since 2.1.2.0:

- `stop_tenant` verifies `tenant_writers_active` reports "all stopped" after
  the shutdown sequence and fails loudly otherwise (WI#133/#134).
- `await_writer_quiescence` polls `tenant_writers_active` every 1s for a
  bounded window (env-overridable `LUNARWING_MT_QUIESCE_INTERVAL`/`DEADLINE`,
  defaults 1s/30s); rc 0 is tolerated until the deadline, unclassifiable state
  (rc 2) still fails closed immediately. Both stop call sites use it.
- OpenRC status classification no longer matches a stopped WeeChat unit's
  "session is not running" text as *running* (WI#137 false-positive), and
  `stopping`/`starting` now count as active so a mid-stop unit is never
  misclassified as quiesced; `crashed` counts as not writing (the container
  branch stays authoritative). Runtime-absent tenants classify as quiesced
  (WI#135 followup).

### Relay rotation start exclusion (2.1.3.0, WI#202)

The daemon, WeeChat adapter, and WeeChat pane now enter through the same
credential start barrier on systemd user units and OpenRC. `render-units`
installs the launcher; `rotate-relay-password` refuses unguarded deployed units.
The barrier covers self-heal's existing `restart_service` dispatch, direct init
starts, and supervisor respawns. Rotation takes its exclusive lock before
stopping consumers; launchers hold a shared lock through password loading until
exec. Starts during the swap fail before a credential consumer runs. A durable
recovery marker inhibits starts after an interrupted rotation or failed
rollback, including across reboot. Self-heal cannot clear that marker.

See [WeeChat relay rotation](../ops/WEECHAT-SERVICES.md#rotate-a-relay-password)
for deployment and recovery. Rotation releases the barrier before restoring
services and clears start-limit/exhausted-supervisor state caused by concurrent
denied restarts.

### WI#180 health-system parity (2.1.2.0)

The WI#180 health-system-parity work extended coverage and remediation without
changing the pipeline structure documented above:

- **opencode2 container-health role:** `health-systemd.sh` now includes
  opencode2 in its container-health role list, so an active-but-unhealthy
  opencode2 container reads critical. `health-openrc.sh` and
  `lunarwing-self-heal.sh` strip the opencode2 role and `-sup` suffix when
  resolving a unit to its tenant, so remediation targets the tenant user bus.
- **Voice role mappings:** voice `-*` units resolve to their tenant across all
  health consumers. Voice follows a monitor-only policy aligned to its service
  identity (WI#180 F6).
- **Monitor-only runtime/knowledge diagnostics:** the `runtime` and `knowledge`
  healthchecks register as monitor-only diagnostics under MT
  `SELF_HEAL_REMEDY_LOGICAL=false`. Per-instance failures escalate as
  `runtime@<tenant>` / `knowledge@<tenant>` — never a restart, no
  `SERVICE_MAP` lookup. Healthy instances clear state via report-as-truth;
  component-level gaps escalate bare; unprobed instances never page per tenant.
- **Gateway hardening:** absent legacy sessions dir is healthy-with-note (not a
  degradation), stale lock and oversized session issues are now detected, and
  the `gateway` suite validates the stale-lock/oversized-session checks.
- **Alert freshness:** `LunarWingHealthReportStale` evaluates
  `time() - report_timestamp_seconds` at scrape time so a stopped exporter
  serving a frozen textfile still trips the alert.
- **Knowledge monitoring is enablement-aware:** reads
  `KNOWLEDGE_SYNC_ENABLED` / `KNOWLEDGE_SWEEP_ENABLED` and cadences from the
  tenant env file; disabled features ignore stale/failed/malformed state.

The canonical aggregate suite list grew from 14 to 19 registered suites (the
WI#180 note initially overcounted at 20; the corrected count is recorded in
`SELF_HEAL_COVERAGE_AUDIT_v2.0.4.0.md`); see that audit for the `ORDER` list.

`ic/scripts/setup-instance.sh` (the deprecated single-instance helper) does not
reference the health pipeline — it predates the MT integration.

## Coverage model — one host install covers all tenants

Self-heal is host-wide and discovers tenants on its own, so it should **not** be
installed per tenant:

- **systemd:** `health-systemd.sh` probes per-tenant *user* units by reading the
  tenant registry (`ports.json`) and querying each tenant user's `--user` bus.
- **OpenRC:** `health-openrc.sh` scans `/etc/init.d/` for `lunarwing-*` (which
  already includes `lunarwing-proxy-*`, `lunarwing-pg-*`, and other tenant
  services) plus `xmpp-bridge-*` — no separate proxy glob or config is needed
  (`docs/ops/MULTITENANCY-PRODUCTION.md` § Health Checks).
- **Remediation:** `lunarwing-self-heal.sh` maps each unhealthy unit back to its
  owning tenant via the registry and restarts it on that user's bus
  (`sudo -u <user> systemctl --user restart …`) or via `rc-service` on OpenRC.

Non-tenant base units use `SELF_HEAL_SYSTEMD_SCOPE`: host-global MT installs use
`system`; manually scheduled single-node user timers must set `user`. Tenant
units ignore this setting and always route through registry owner user bus.
Both base-unit health probes and remediation honor the selected scope.
Host-global NetBird daemon always uses system bus when restart is delegated.

## Net effect on a fresh MT host

When provisioning a tenant via `lunarwing-mt-admin.sh add-tenant` (the default,
no `--no-health`), the host-global health-check → self-heal pipeline is installed
and scheduled in one step: the operator does not need to separately schedule it.
The pipeline auto-discovers all current and future tenants, so adding or removing
tenants requires no self-heal changes.

The watchdog installer (`install-lunarwing-watchdog.sh`) remains a separate,
manually-run step for the **service-level watchdog** (restarts `lunarwing.service`
if down). On systemd and OpenRC, it also copies the self-heal scripts but does not schedule them —
that is handled by `add-tenant`. On hosts that have not run `add-tenant` (e.g.
single-instance setups), the operator must schedule the pipeline manually per
`ic-infrastructure-health-check/README.md`.

## Historical gaps (resolved in v1.1.9)

The following gaps were documented before `ensure_health_pipeline()` was added
to `add-tenant`:

- **G1 — self-heal installed but not scheduled.** On systemd and OpenRC,
  `install-lunarwing-watchdog.sh`
  copies `lunarwing-self-heal` and `lunarwing-health-cron` to `/usr/local/sbin`
  but enables no timer for them. **Resolved:** `add-tenant` now calls
  `ensure_health_pipeline()` which schedules the pipeline via systemd timer
  or OpenRC cron.
- **G2 — no provisioning hook.** There was no `mt-admin` flag or host-bootstrap
  step that ran the watchdog installer or scheduled the health pipeline.
  **Resolved:** `add-tenant` now calls `ensure_health_pipeline()` automatically;
  `--no-health` can opt out.
