# WI#734 — LunarDepth external-worker test, third attempt (`lunardepthtester`, AJ edition)

> Run started 2026-11-16 20:36Z by Applejack on branch
> `aj/kimi-wi734-20261116-2031`, from the target
> `aragents/kimil00p/v2.2.0.0` at `4473ae365`.
> This is the third attempt at the WI#720 exercise. The first (!2487) was closed
> unmerged; the second (!2497, `5ac73beaf`) recorded a blocked run in
> `docs/ops/WI734_TEST_TENANT_LUNARDEPTH_AJ.md` and left boxes 3 and 4 of this
> item open. That document is kept as it was; this one records the attempt that
> set out to close those two boxes. The goals file is deliberately unchanged;
> this is a work-item run.

## Scope

Work item #734 asks for a tenant named `lunardepthtester` with the LunarDepth
external worker and `--with-toolchains`, jobs of medium to high complexity, and
a record of the results and every hiccup. Boxes 1, 2, 5 and 6 were met by
!2497. This attempt has to meet the two that were left:

3. the tenant has the LunarDepth worker **running** with `--with-toolchains`,
   with the build result and the runtime check in this report;
4. jobs of medium to high complexity ran, with their results.

The constraint that shaped the run is §3 of the item: the run has to be
finished before the MR is opened. The work order was therefore host record →
worker image → runtime audit → jobs → this document.

## Host and preflight

Recorded before the first `mt-admin` command of this attempt, on the host this
run used:

| Fact | Value |
| --- | --- |
| Distribution | Arch Linux (rolling), per `/etc/os-release` |
| PID 1 | `systemd` (`/proc/1/comm`) |
| `rc-service` | absent (`command -v rc-service` → not found) |
| `systemctl` | `/usr/sbin/systemctl` |
| Kernel | `6.18.54-2-lts` |
| Podman | `podman version 6.2.0` |
| CPUs | 12 (`nproc`); every build confined with `taskset -c 0-11` |
| Health pipeline | `/etc/lunarwing/health.env` reads `LUNARWING_SERVICE_MANAGER=systemd` |
| Tenants in the registry | `hermes`, `lunardepthtester` |

No command of this run sets `LUNARWING_SERVICE_MANAGER`. The tenant's units are
systemd **user** units under the tenant account, created by the previous attempt
and found in place; this run created no unit.

### State in which the tenant name was found

Read-only, before anything was done:

- `mt-admin status lunardepthtester`: tenant present; gateway `10010`
  listening (`{"status":"healthy","channel":"gateway"}`); PostgreSQL `running`
  (`lunarwing-pg-lunardepthtester`); `LunarDepth worker: not created`;
  `lunarwing-worker-lunardepth:latest: missing`; LunarVision healthy
  (20015/20016); user unit `lunarwing-lunardepthtester.service` active.
- `/etc/lunarwing/ports.json` holds `lunardepth_base 52010` with
  `lunardepth_wss 52010` and `lunardepth_health 52011`.
- The tenant clone is at `d872e9632`, the second attempt's report commit.
- No `lunarwing-worker-lunardepth` image in the root store or in the tenant's
  store.

The tenant was **reused as it stands**. Nothing was removed, no account or
registry entry was recreated, and no new tenant was made. The maintainer was
asked on this item in note 18277 (20:02Z) with three options; no answer had come
by 20:39Z, so the run took the recommended option 2 (reuse, worker work only),
which removes nothing and adds no workaround on the host. The decision is
recorded in note 18295.

## Worker image

<!-- RESULTS: build -->

## Runtime audit

<!-- RESULTS: runtime -->

## Job exercise

<!-- RESULTS: jobs -->

## Findings and hiccups

<!-- RESULTS: findings -->

## Host changes this run made

<!-- RESULTS: host-changes -->

## Acceptance

<!-- RESULTS: acceptance -->
