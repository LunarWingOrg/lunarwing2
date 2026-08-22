# Goal 28 (v2.0.8.0): Health systems — systemd pass

Date: 2026-08-22 · Branch: `aj/kimi-28-20260822-0001`

Scope: thoroughly test and improve the health systems' **systemd** surface
(item 27 covers the OpenRC/Gentoo side). All work verified on the dev host via
the mock-based harnesses (no real system bus is ever touched by the suites).

## Baseline

`ic-infrastructure-health-check/tests/run-all.sh`: **575/575 green** across 11
suites before any change (regression, matrix, chaos, openrc, systemd,
lunarvision, init-templates, lunarvoice, voice-init-templates, netbird,
metrics-exporter).

## Gaps found and fixed

1. **Watchdog had no bus-scope support** (`ic/scripts/lunarwing-watchdog.sh`).
   Single-node installs where `lunarwing.service` runs under the installing
   user's systemd manager were invisible to the watchdog: every hourly probe
   hit the system bus, never found the unit, and attempted doomed system-bus
   restarts. Added `LUNARWING_WATCHDOG_SYSTEMD_SCOPE` (`system`|`user`,
   default `system`, invalid values warn + fall back) dispatching every
   `systemctl` call — parity with `SELF_HEAL_SYSTEMD_SCOPE` in the
   health/self-heal pipeline. Documented in
   `ic/systemd/lunarwing-watchdog.service` and
   `docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md`.

2. **Watchdog installer could abort after a successful install**
   (`ic/scripts/install-lunarwing-watchdog.sh`). The smoke
   `systemctl start lunarwing-watchdog.service` at the end of install exits 1
   whenever the monitored service can't be restarted yet (e.g. LunarWing not
   yet installed); under `set -e` that killed the installer *after* the timer
   was enabled, reporting failure for a fully installed watchdog. The smoke
   run and trailing `status` are now non-fatal with a journal pointer.

3. **Zero watchdog test coverage.** New suite
   `ic-infrastructure-health-check/tests/test-watchdog-systemd.sh`
   (32 assertions, state-file-driven systemctl/curl mocks): active no-op,
   inactive restart+verify, StartLimitBurst `reset-failed` ordering, restart
   command failure, stuck-inactive, gateway deep-check (unhealthy channel →
   restart; healthy / unconfigured / unreachable gateway → no spurious
   restart), user-scope bus routing, invalid-scope fallback, lock contention.
   Registered as `watchdog` in `run-all.sh`.

4. **MT health oneshot had no explicit runtime bound**
   (`ic/scripts/mt-admin-lib/12-health-pipeline.sh`). The
   `lunarwing-mt-health.service` oneshot inherited the ~90s distro default
   `TimeoutStartSec`, which can kill a legitimate multi-tenant remediation
   mid-restart, and a wedged tenant bus could stall the pipeline without an
   explicit bound. Now `TimeoutStartSec=max(900s, one full interval)`;
   `Persistent=true` replays a killed run. Unit dir overridable via
   `LUNARWING_MT_SYSTEMD_UNIT_DIR` (symmetric in `remove_health_pipeline`),
   which let `test-mt-admin-health-interval.sh` gain the first
   systemd-rendering coverage: +8 assertions (unit files rendered, cadence
   follows interval, bound scales past the floor, `Persistent=true`).

5. **Timer monitoring blind spot** (`health-systemd.sh`). An `active`/`waiting`
   timer whose next wake systemd never computed (empty/`0`/`n/a` — e.g. an
   unschedulable calendar spec) will never fire but reported **healthy**,
   silently stopping whatever pipeline it drives. Now classified `degraded`
   with an explicit issue; gated to `waiting` (a timer whose oneshot is
   mid-run may legitimately recompute) and never downgrades a worse verdict.
   Mock `show` serves `LastTriggerUSec`/`NextElapseUSecRealtime` via an
   optional `<unit>.elapse` sidecar; +11 assertions (sections U–X: scheduled,
   `0`, empty, mid-run, inactive). Suite 57 → 68 assertions.

6. **Self-heal tenant user-bus calls were unbounded**
   (`ic-infrastructure-health-check/lunarwing-self-heal.sh`). The health check
   bounded the same probes at 5s, but `_systemd_user_restart`,
   `_systemd_user_active`, and the tenant branch of `service_substate` could
   hang the whole remediation tick on a dead session bus. Each call now runs
   under `timeout -k 2` (`SELF_HEAL_SYSTEMD_PROBE_TIMEOUT`, default 15s,
   graceful fallback when `timeout(1)` is missing). A bounded hang is a normal
   failed restart: retry counted, backoff scheduled, tick completes. Chaos
   harness CH16 proves it with a sleeping-sudo mock (+4 assertions). Knob
   documented in `config.sh.example` and the README table.

7. **Metrics-listener systemd unit had no validation.** New suite
   `ic-infrastructure-health-check/tests/test-health-metrics-unit.sh`
   (15 assertions): pins the loopback/read-only contract (`DynamicUser`,
   `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`, no
   `ReadWritePaths`, no wildcard bind, no committed token, optional
   `EnvironmentFile`) and runs `systemd-analyze verify` when available
   (binary substituted with `/bin/true` so directives, not host state, are
   validated). Registered as `metrics-unit`.

## Verification

- `run-all.sh`: **637/637 green** across 13 suites (575 baseline + 62 new).
- `ic/scripts/tests/test-mt-admin-health-interval.sh`: all pass (12
  assertions incl. the new systemd-rendering ones).
- Adjacent mt-admin harnesses re-run green: `test-add-tenant-env-flags.sh`,
  `test-workload-isolation-rendering.sh`.
- `taskset -c 0-11 cargo test -j12 -p lunarwing_health_metrics`: 4/4 pass.
- `bash -n` clean on every touched script. `check-mt-admin.sh` could not run
  (shellcheck not installed on this host); no new shellcheck-prone constructs
  were introduced (arrays/case/printf only).

## Not changed (deliberately)

- Base-unit probes in `health-systemd.sh` stay unbounded per-call: the driver
  already bounds the whole component at 30s and reports `unknown`; converting
  a wedged system bus into per-unit "not found: critical" would misreport.
- No hardening added to the MT health oneshot beyond the timeout: it
  legitimately needs root + `sudo` into tenant buses, so `NoNewPrivileges` et
  al. would break remediation.
- `health-metrics` listener Rust code untouched (tests green as-is).
