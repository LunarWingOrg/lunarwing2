<!-- study-notes-metadata
{
  "confidence": 0.9,
  "source_commit": "63127b12",
  "stale_after_days": 90,
  "status": "active",
  "studied_at": "2026-08-22T01:06:22.240852+00:00",
  "tags": [
    "item-28",
    "health-systems",
    "systemd",
    "openrc-transfer",
    "watchdog",
    "self-heal"
  ]
}
-->

# Item 28 systemd health pass — findings + OpenRC (item 27) transfer list

# Item 28 (systemd health pass) — findings transferable to item 27 (OpenRC)

Branch aj/kimi-28-20260822-0001, MR !615, 655 insertions. Baseline 575/575 green; all new suites mock-based, no real bus touched.

## What landed (systemd side)
1. **Watchdog bus-scope dispatch** — `LUNARWING_WATCHDOG_SYSTEMD_SCOPE` (system|user, invalid→warn+fallback) via `sctl()` wrapper around every systemctl call. Parity with existing `SELF_HEAL_SYSTEMD_SCOPE`.
2. **Installer no longer aborts post-success** — smoke `systemctl start` of the oneshot exits 1 when lunarwing.service isn't installed yet; under `set -e` that killed the installer AFTER the timer was enabled. Now non-fatal w/ journal pointer.
3. **Timer blind-spot detection** (health-systemd.sh) — active+waiting timer with empty/0/n-a NextElapseUSecRealtime never fires but passed as healthy; now → degraded. Only in `waiting` substate, never downgrades a worse verdict.
4. **Self-heal probe bounds** — `_sh_timeout()` (timeout -k 2, `SELF_HEAL_SYSTEMD_PROBE_TIMEOUT` default 15s) around all `sudo -u <t> systemctl --user` calls; wedged user-manager/bus can no longer stall a self-heal tick. Graceful unbounded fallback when timeout(1) absent.
5. **MT health oneshot TimeoutStartSec** — floor of max(interval, 900s); ~90s distro default killed legit multi-tenant remediations mid-restart. Also `LUNARWING_MT_SYSTEMD_UNIT_DIR` override (install + remove paths).
6. **New suites**: test-watchdog-systemd.sh (32 assertions incl. StartLimitBurst reset-failed ordering, gateway deep-check false-restart guards, user-scope routing, lock contention), test-health-metrics-unit.sh (15), mt-admin health-interval harness coverage.

## Transferable to OpenRC #27 (Rarity's host)
- **Pattern 2 is init-agnostic**: `install_openrc_watchdog()` in the same installer likely has the same abort-after-success class (check `rc-service start` smoke under set -e).
- **Pattern 4 applies to ANY init**: OpenRC `rc-service`/`supervise-daemon` calls on a dead daemon can hang identically — bound them with the same timeout helper philosophy.
- **Pattern 3 has an OpenRC analog**: fcron/cron entries that silently stopped, or `started` status with dead supervise-daemon — the *concept* (status reads healthy but nothing will ever fire) deserves an OpenRC-specific check.
- **Zero watchdog test coverage was systemic**: the OpenRC watchdog path was equally untested — mirror the mock-suite approach (the existing openrc test suite shows the rc-mock pattern).
- Watchdog `sctl()` dispatch pattern maps to nothing needed on OpenRC (no dual bus) — skip.
