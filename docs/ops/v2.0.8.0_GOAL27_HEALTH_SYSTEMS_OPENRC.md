# Goal 27 (v2.0.8.0): Health systems — OpenRC/Gentoo pass

Date: 2026-08-22 · Branch: `rarity/kimi-27-20260822-0901`

Scope: thoroughly test and improve the health systems' **OpenRC/Gentoo**
surface, transferring the applicable item-28 (systemd pass) findings — see
`docs/ops/ajsystemdhealthpass.md` for the transfer list and
`docs/ops/v2.0.8.0_GOAL28_HEALTH_SYSTEMS_SYSTEMD.md` for the systemd side.
All work verified on the dev host via the mock-based harnesses (no real OpenRC
is ever touched by the suites).

## Baseline

`ic-infrastructure-health-check/tests/run-all.sh`: **635/635 green** across 13
suites before any change.

## Transfer-list verification and what landed

1. **Pattern 4 (bound init calls) — applied twice.**
   - `lunarwing-self-heal.sh`: the OpenRC remediation paths (`_openrc_active`,
     `_openrc_restart`) were the only init calls still unbounded — a wedged
     supervise-daemon or hung `rc-service` could stall an entire remediation
     tick. They now run under the same `_sh_timeout` wrapper as the systemd
     tenant-bus probes. The knob was generalized: `SELF_HEAL_PROBE_TIMEOUT`
     (default 15s), with `SELF_HEAL_SYSTEMD_PROBE_TIMEOUT` honoured as a
     legacy alias. Documented in `config.sh.example` and the README table.
     Chaos harness CH17 proves a wedged (sleeping) `rc-service` is counted as
     a failed restart with the tick completing inside the bound.
   - `ic/scripts/lunarwing-watchdog-openrc.sh`: every `rc-service`
     status/start/restart call is now bounded by `timeout -k 2`
     (`lunarwing_watchdog_probe_timeout` / `LUNARWING_WATCHDOG_PROBE_TIMEOUT`,
     default 15s, non-numeric falls back to 15, unbounded fallback when
     `timeout(1)` is absent). This mattered doubly here: the watchdog holds
     its flock for the whole run, so one hung `rc-service` would silently
     wedge every future hourly run (they exit early on lock contention).
     Knob documented in `ic/systemd/lunarwing-watchdog.confd`.

2. **Pattern 2 (installer abort-after-success) — verified absent; parity gap
   closed.** `install_openrc_watchdog()` had no such bug: every post-install
   `rc-update`/`rc-service` scheduler call was already `|| true`. But unlike
   the systemd path it never smoke-ran the installed wrapper, so a broken
   wrapper/confd would only surface an hour later via a silent no-op cron.
   The installer now smoke-runs the wrapper once (warn-only, never aborts —
   same philosophy as the item-28 systemd fix).

3. **Pattern 3 (blind spot) — OpenRC analog implemented.** The systemd check
   flagged a `waiting` timer that will never fire. The OpenRC analog: the
   watchdog and the MT health pipeline are cron-driven (hourly hook under
   `/etc/cron.hourly` / `/etc/periodic/hourly`, or an fcron/crontab managed
   block), so a stopped or missing cron scheduler daemon means *nothing ever
   fires* while every per-service probe still reads healthy.
   `health-openrc.sh` now reports `metrics.scheduler`
   (`hook_installed`/`running`/`status`) and raises the component to
   `degraded` with an explicit issue when a LunarWing cron hook is installed
   but no scheduler (`fcron cronie dcron crond busybox-cron`) is running.
   Design constraints honoured: report-only (the scheduler is never listed in
   `.metrics.services[]`, so self-heal will not try to restart a
   pseudo-service), monotonic (degraded at worst — never overrides a critical
   per-service verdict), fail-safe (inert when no hook is installed, so hosts
   that schedule the pipeline some other way are not false-flagged), and
   bounded (tab reads go through the same `timeout` policy as status probes;
   hook dirs overridable via `HEALTH_OPENRC_CRON_DIRS`).

4. **Zero watchdog test coverage — mirrored.** New suite
   `ic-infrastructure-health-check/tests/test-watchdog-openrc.sh` (28
   assertions, state-file-driven `rc-service`/`curl` mocks): started no-op,
   stopped start+verify, start failure, stuck-stopped, gateway deep-check
   guards (unhealthy → restart; healthy / unconfigured / unreachable → no
   spurious restart), wedged-`rc-service` bounded run, non-numeric timeout
   fallback, lock contention. Registered as `watchdog-openrc` in `run-all.sh`.
   Also restored the executable bit on `lunarwing-watchdog-openrc.sh` in git
   (parity with the systemd wrapper; the installer already installed it 0755).

5. **Watchdog `sctl()` bus-scope dispatch — skipped** (no dual bus on OpenRC),
   as recommended by the transfer list.

## Verification

- `run-all.sh`: **691/691 green** across 14 suites (635 baseline + 56 new:
  28 watchdog-openrc, 24 openrc S16–S22, 4 chaos CH17).
- `bash -n` clean on every touched script.
- Test hermeticity: `test-health-openrc.sh` now shadows `crontab`/`fcrontab`
  with fixture-driven mocks and sandboxes the cron-hook dirs, so the host's
  real cron setup can never leak into a scenario (this host has both binaries
  installed — without the mocks the new check would read host state).

## Not changed (deliberately)

- The dead-scheduler finding is **not** auto-remediated by self-heal: starting
  a host cron daemon is an operator decision, and keeping it out of
  `.metrics.services[]` guarantees no pseudo-service restart attempts.
- `install_openrc_watchdog`'s `fcrontab "$tmp" root` stays fatal under
  `set -e`: if the tab cannot be written the schedule genuinely was not
  installed — that is a real install failure, not an abort-after-success.
- OpenRC container `-sup` babysitter units and mt-admin health-pipeline
  rendering untouched (covered by their own harnesses).
