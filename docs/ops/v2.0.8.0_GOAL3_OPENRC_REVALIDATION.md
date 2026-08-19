# Goal 3 (work item #54) — OpenRC cgroup validation re-run against refactored mt-admin

**Date:** 2026-08-19 (18:05–18:15 UTC)
**Branch:** `rarity/kimi-3-20260819-1801`
**Reason for re-run:** item 9 split `ic/scripts/lunarwing-mt-admin.sh` into a thin
composition root plus 14 `ic/scripts/mt-admin-lib/*.sh` modules, so the
2026-08-19 00:01 validation (branch `rarity/kimi-3-20260819-0001`, recorded in
that branch's `O-DETAILS.md`) no longer covers the shipped code layout. The
refactor was a byte-identical line-range move (375 function bodies verified
programmatically on that branch), but goal 3's acceptance gate requires the
host validation to pass against the current code.

## Environment

- Host: Gentoo, OpenRC 0.63.1, cgroup v2 (`cgroup2fs`); root controllers and
  root `cgroup.subtree_control` = `cpuset cpu io memory hugetlb pids rdma misc dmem`.
- Code under test: refactored mt-admin — root `ic/scripts/lunarwing-mt-admin.sh`
  (1,264 lines) + `mt-admin-lib/00..13` modules. Cgroup-relevant code lives in
  `08-units-openrc.sh` (`_lunarwing_service_cgroup_cleanup`,
  `rc_cgroup_cleanup` rendering), `05-tenant-build.sh` (`_run_tenant_build` /
  `_run_tenant_build_cgroup`), `13-cgroups-openrc.sh` (`setup-cgroups`).
- Tenant: `henry` (restarted/stopped/started by the procedure; not torn down).
  Pre-delegated subtree `/sys/fs/cgroup/lunarwing-henry` with
  `cgroup.subtree_control = memory pids` present from goal 1's `setup-cgroups`.

## Section 1 — cgroup v2 and OpenRC cleanup support: PASS

- `stat -fc %T /sys/fs/cgroup` = `cgroup2fs`; OpenRC 0.63.1.
- `/usr/libexec/rc/sh/rc-cgroup.sh` `cgroup_running()` (lines 50–55) still
  checks only `unified/${RC_SVCNAME}`, `${RC_SVCNAME}`, and
  `openrc/${RC_SVCNAME}` — never `openrc.${RC_SVCNAME}`. OpenRC issue #910 is
  still present on this host (no backport of upstream commit `1cf3944`), so
  the LunarWing rendered fallback remains the required cleanup path.
- Rendered `/etc/init.d/lunarwing-henry` contains
  `_lunarwing_service_cgroup_cleanup`, `cgroup.kill`, and the bounded
  `cgroup.procs` reaping fallback.

## Section 2 — render, restart, effective service limits: PASS

- `render-units henry` and `restart-tenant henry` both ran through the
  post-split module path without error.
- `/sys/fs/cgroup/openrc.lunarwing-henry` read back exactly the defaults:
  - `memory.high = 6442450944` (6 GiB)
  - `memory.max  = 8589934592` (8 GiB)
  - `cpu.max     = 800000 100000` (800%)
  - `pids.max    = 2048`
- `cgroup.procs` contains `supervise-daemon` (root, ppid 1) and the
  `lunarwing` daemon (user `henry`); both report `0::/openrc.lunarwing-henry`
  via `/proc/<pid>/cgroup`.
- Daemon start implies the rendered `start_pre` effective-limit verification
  passed (startup fails when any configured value does not verify).

## Section 3 — stop cleanup with controlled leftover process: PASS

- A controlled root `sleep 600` (pid 189834) was moved into
  `/sys/fs/cgroup/openrc.lunarwing-henry` (confirmed in `cgroup.procs`).
- `stop-tenant henry` reaped it: `kill -0` after the stop reported the process
  gone (PASS). Since this host's OpenRC lacks commit `1cf3944`, the reaping
  was performed by the LunarWing rendered portable fallback.
- `start-tenant henry` afterwards returned the tenant to full service.

## Section 4 — temporary operator-build memory limits: PASS

Probe 1 (`sleep 60`, run via tmux with
`source <root script>; _run_tenant_build henry "sleep 60"`):

- Build cgroup appeared at `/sys/fs/cgroup/lunarwing-henry/build` — the
  pre-delegated-subtree path, not the legacy root-level `lunarwing-build-henry`.
- `memory.high = 17179869184` (16 GiB), `memory.max = 25769803776` (24 GiB).
- `cgroup.procs` non-empty: the `sleep 60` process ran as user `henry` with
  `/proc/<pid>/cgroup` = `0::/lunarwing-henry/build` for the full minute
  (go-file rendezvous + root-side `setpriv` migration).
- On completion the build cgroup and sync dir were removed and no processes
  were left over (`pgrep -u henry sleep` empty).

Probe 2 (`echo ran >> marker; exit 2` — regression for finding #2, the old
status-2 double-run bug):

- Command ran exactly once (marker gained exactly one line per run).
- Exit status 2 propagated out of `_run_tenant_build` (captured `EXIT=2`).
  Note for future probes: the mt-admin root script sets `set -euo pipefail`
  at global scope (line 4), so a sourced probe shell must `set +e` before
  calling `_run_tenant_build` if it wants to capture a nonzero status instead
  of dying on it. This is a probe-harness detail, not an mt-admin defect.
- Build cgroup cleaned up afterwards.

## Fixture harness

- `ic/scripts/tests/test-workload-isolation-rendering.sh`: **94/94 PASS**
  (fixture + runtime-helper coverage: strict build setup, exit-status
  propagation, conf.d override preservation, rerender idempotence, local.d
  boot render, setup-cgroups end-to-end fixture, cpu quota conversion,
  systemd build scoping).

## Tenant preservation

After sections 2–4, `mt-admin status henry` verified all expected services
started (core, pg, pg-sup, vision, vision-sup, weechat-adapter, xmpp-bridge);
LunarVision healthy on API/health ports 20035/20036. No sidecar image drift
was observed this run (the root podman store still holds the vision image;
the 00:01 run's `podman save|load` repair persisted).

## Acceptance gate

All conditions from `O-DETAILS.md` are met against the **refactored** mt-admin:

- The four high/medium findings remain fixed (code moved byte-identically into
  the module layout; runtime behavior re-proven above).
- Fixture/runtime regression coverage passes (94 checks).
- OpenRC 0.63.1 on this host still lacks upstream commit `1cf3944`; the
  LunarWing rendered fallback supplies the cleanup (section 3).
- All four host-validation sections passed on 2026-08-19 with tenant `henry`
  against the post-split code layout.

**Goal 3 re-validation: PASS.**
