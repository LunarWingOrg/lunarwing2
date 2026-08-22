# Infrastructure Self-Healing — Deployment & Provisioning Wiring

**Date:** 2026-06-13 (revalidated 2026-08-05)
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
`lunarwing.service` if down). On systemd and OpenRC, it also copies the self-heal scripts into
`/usr/local/sbin` but does **not** schedule them — that scheduling is now handled
by the `add-tenant` `ensure_health_pipeline()` flow.

> **Scheduling ownership:** the self-heal pipeline is scheduled exclusively by
> `add-tenant` / `ensure_health_pipeline()`. The `install-lunarwing-watchdog.sh`
> installer copies the self-heal scripts to `/usr/local/sbin` on systemd and
> OpenRC but will never
> schedule them. On a host where the watchdog installer ran but `add-tenant`
> hasn't, the scripts are on disk but unscheduled.

## Two distinct "watchdogs" (don't conflate them)

| | Service-level watchdog | Infrastructure self-heal |
|---|---|---|
| Code | `ic/scripts/lunarwing-watchdog*.sh` | `ic-infrastructure-health-check/lunarwing-self-heal.sh` |
| Scope | Restarts the base `lunarwing.service` if down | Reads health-check reports, remediates any unhealthy component/unit (incl. per-tenant), with grace / backoff / flap-guard / escalation |
| Scheduled by | `lunarwing-watchdog.timer` (enabled by the installer) | `lunarwing-mt-health.timer` / managed cron (scheduled by `add-tenant` via `ensure_health_pipeline()`) |
| Unit | `ic/systemd/lunarwing-watchdog.{service,timer}`; `ExecStart=/usr/local/sbin/lunarwing-watchdog` | `lunarwing-mt-health.{service,timer}` (systemd) or managed fcron/crontab entry (OpenRC) |

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

`ic/scripts/install-lunarwing-watchdog.sh` (run as root on systemd/OpenRC —
launchd runs rootless as a user agent — **once per host**;
auto-detects systemd / OpenRC / launchd) is the installer for the **service-level
watchdog**. On systemd it:

- installs + `enable --now`s `lunarwing-watchdog.timer` and the
  `lunarwing-watchdog.service` → `/usr/local/sbin/lunarwing-watchdog`
  (the **service-level** watchdog), and
- *conditionally* (the "D-1" block) copies the self-heal pieces into place:
  - `lunarwing-self-heal.sh` → `/usr/local/sbin/lunarwing-self-heal`
  - `cron-wrapper.sh`        → `/usr/local/sbin/lunarwing-health-cron`

The OpenRC path mirrors this. On systemd hosts the watchdog probes the system
bus by default; for single-node installs where `lunarwing.service` runs under
the installing user's manager, set `LUNARWING_WATCHDOG_SYSTEMD_SCOPE=user`
(mirroring `SELF_HEAL_SYSTEMD_SCOPE`) so every probe/restart uses
`systemctl --user` — invalid values warn and fall back to `system`. **Note:**
the watchdog installer does **not**
create or enable a timer for the health-cron — that scheduling is now handled by
the `add-tenant` `ensure_health_pipeline()` flow (Path 1 above). On hosts where
`add-tenant` hasn't run, scheduling `cron-wrapper.sh` remains a **manual** step
documented in `ic-infrastructure-health-check/README.md`.

## Multi-tenant provisioning now installs it

`lunarwing-mt-admin.sh add-tenant <name>` calls `ensure_health_pipeline()` as
the final step of tenant provisioning (unless `--no-health` is passed). The full
`add_tenant()` flow (`mt-admin-lib/04-tenant-lifecycle.sh`):

1. Allocate a port block (registry)
2. Create the tenant OS user
3. Clone the tenant repo
4. Write env/config files (vision / voice / LunarWing / bridge / optional proxy
   and DarkIRC / Gotify / external workers)
5. Bootstrap the WeeChat relay, provision the SSH harness, optionally wire the
   Study Notes MCP (`--with-study-mcp`), and build the vision sidecar image if
   it is missing
6. Start the per-tenant PostgreSQL container
7. Render the per-tenant init units (`lunarwing-<name>`, `xmpp-bridge-<name>`,
   `lunarwing-proxy-<name>`, weechat adapter, …)
8. On non-systemd inits, set up the pre-delegated cgroup v2 subtree (warn-only;
   repairable later via `setup-cgroups`)
9. **Call `ensure_health_pipeline()`** — installs + schedules the host-global
   health-check → self-heal pipeline (covers all tenants automatically).

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
