# Infrastructure Self-Healing — Deployment & Provisioning Wiring

**Created:** 2026-06-13. **Source revalidated:** 2026-11-01, v2.1.8.0.
**Status:** Reference — documents current behavior, not a proposal.
**Related:** [health pipeline README](../../ic-infrastructure-health-check/README.md),
[multi-tenant operations](../ops/MULTITENANCY-PRODUCTION.md),
[historical chaos plan](../internal/history/proposals/CHAOS_ENGINEERING_TEST_PLAN.md).

Current ownership: `ensure_health_pipeline`, `health_env_reconcile_line`, and
`refresh_deployed_health_pipeline` in `ic/scripts/mt-admin-lib/12-health-pipeline.sh`;
tenant provisioning/upgrade in `04-tenant-lifecycle.sh`; runtime egress policy in
`15-worker-egress.sh`. WI#424 adds a monitor-only worker-egress healthpoint and
refreshes already-installed host health assets during tenant upgrades. Init
operations remain inside mt-admin and the dedicated health tooling; tenant
systemd units use the tenant user manager, while the host health timer uses the
system manager.

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

> **Scheduling ownership:** `ensure_health_pipeline()` owns the schedule,
> reached by `add-tenant`, the fleet enabler, and refresh of an already-installed
> pipeline during upgrade. `install-lunarwing-watchdog.sh` only copies the
> self-heal scripts on systemd/OpenRC; that copy alone does not schedule them.

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
3. Writes `/etc/lunarwing/health.env` (mode 0600) if absent, including the base
   dir, init system, registry path, `SELF_HEAL_SYSTEMD_SCOPE=system`, disabled
   NetBird probing, and MT flags. Re-renders preserve operator edits while
   reconciling `LUNARWING_SERVICE_MANAGER`, `SELF_HEAL_TENANTS_FILE`,
   `HEALTH_WORKER_EGRESS_RUNTIME`, and `HEALTH_WORKER_EGRESS_ROOTLESS`.
   A stale `LUNARWING_MT_ADMIN` checkout path is removed: root cron runs the
   installed read-only `health-worker-egress.sh`, never a user-owned checkout.
   Egress launch-mode updates use `health_env_reconcile_line` with a mode-0600
   same-directory scratch file and atomic rename, preserving file mode/owner.
4. Writes a launcher script that sources the env file and execs `cron-wrapper.sh`.
5. Schedules the launcher:
   - **systemd**: installs + enables `lunarwing-mt-health.{service,timer}`
     (default: every 15 min). The oneshot is explicitly bounded with
     `TimeoutStartSec=max(900s, interval)` — the ~90s distro default can kill a
     legitimate multi-tenant remediation mid-restart, and a wedged tenant user
     bus must never stall the pipeline forever. `Persistent=true` catches up a
     calendar activation missed while the timer was inactive; it does not replay
     a failed/killed oneshot immediately. The next scheduled run observes health
     again. Per-tenant user-bus calls are individually bounded too
     (`SELF_HEAL_SYSTEMD_PROBE_TIMEOUT`, default 15s).
   - **OpenRC**: installs a managed `fcron`/`crontab` entry every 15 min
     (managed block).

   Both init systems default to 15 min. Set
   `LUNARWING_MT_HEALTH_INTERVAL_MIN=N` in `/etc/lunarwing/health.env` to
   persist a different cadence across tenant additions, upgrades, and
   `enable-health-fleet.sh` runs. Use a plain, unquoted `NAME=value` line
   without an inline comment. The same-named environment variable takes
   precedence for a one-off installation and is not written back to the file.
   Every installation logs the effective interval and its source. Values must
   be integers from 1 to 59; invalid values abort installation and name their
   source.

   **Container healthchecks are separate.** On systemd hosts, Podman runs each
   tenant container's `HealthCmd` from its own systemd timer, and every run is
   a full `podman healthcheck run` process. Postgres, LunarVision, LunarVoice,
   and the worker bridge and task containers all use one cadence, 60 seconds by
   default (10 seconds for Postgres and 15 for the others before 2026-11-08;
   dozens of containers on that cadence caused host-wide CPU bursts). Set
   `LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC=N` in the same file, as a plain
   `NAME=value` line, to change it. The same-named environment variable takes
   precedence for one run. Values must be integers from 10 to 3600; invalid
   values stop mt-admin before any unit is written. The health pipeline only
   escalates `unhealthy`, so a longer cadence delays detection by roughly
   three intervals (three retries) and nothing else. Containers whose Quadlet
   is hashed (LunarVision, LunarVoice, workers) are recreated with the new
   cadence on the next `start-tenant` or `upgrade-tenant`; Postgres picks it up
   on its next restart. OpenRC hosts disable Podman's timer healthchecks
   entirely, so the setting does not apply there.

Because the pipeline is host-global and auto-discovers tenants, scheduling is
idempotent. Subsequent `add-tenant` calls re-run `ensure_health_pipeline()`.
`enable-health-fleet.sh` is the explicit fleet installation path.
`upgrade-tenant` calls `refresh_deployed_health_pipeline`, which refreshes and
reconciles only when the managed launcher exists; an old script copy alone does
not re-arm a removed schedule.

Worker-egress failures use monitor-only `worker_egress@<tenant>` escalation,
including when `SELF_HEAL_REMEDY_LOGICAL=false`. The pipeline does not repair
firewall policy. Network creation, fail-closed launch checks, and policy repair
belong to mt-admin's worker-egress module. See
[worker operations](../ops/WORKER-CONTAINERS.md) for the audit and repair surface.

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
