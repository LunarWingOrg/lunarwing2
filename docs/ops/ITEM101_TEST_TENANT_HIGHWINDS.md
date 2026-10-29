# Item 101 (v2.1.7.0) — persistent test tenant `highwinds`

Status: provisioning and verification in progress; item 101 is not complete.

This is a new v2.1.7.0 test pass, not the previous silverzoom or mistyfly run.
Rarity is working sequentially on `aj/kimi-101-20261029-0054`, starting from
`a83ea3d8b`, on 2026-10-29. No subagents are used.

## Completion contract

- Create a new tenant named `highwinds` (a non-animal name from the WI#273
  test-tenant naming convention).
- Enable kimi and pebble external workers with full toolchains.
- Assign complex tasks through LunarWing, independently verify the resulting
  artifacts, and record failures as well as successes.
- Leave the tenant running; do not invoke teardown.

## Host and initial diagnostics

The current host is `ajhermes-z`, Linux `6.18.54-2-lts`, with systemd and
Podman, not the Gentoo/OpenRC host used for the mistyfly report. The only
registered tenant at the start is `hermes`, gateway port 10040. Its daemon,
PostgreSQL, and LunarVision services are active; no workers are selected.
The operator checkout is clean. The filesystem has 264 GiB available and
`nproc` reports 12 usable processors.

The standard mt-admin doctor reports 33 passes and 7 failures:

- `newuidmap` and `newgidmap` are not setuid, but carry `cap_setuid=ep` and
  `cap_setgid=ep`, respectively. A real `podman unshare true` as the existing
  tenant succeeds. The doctor's setuid-only check is therefore not sufficient
  evidence of a broken rootless installation on this host. No helper mode or
  host security setting was changed.
- Root's administrative image store is empty (pebble, opencode, kimi, vision,
  and sandbox image checks fail). The existing tenant's separate rootless
  store does contain working PostgreSQL and LunarVision images. New worker
  images and the standard image-preparation path still need verification.

The provisioning path is `ic/scripts/lunarwing-mt-admin.sh` with explicit
`LUNARWING_SERVICE_MANAGER=systemd` and
`LUNARWING_CONTAINER_RUNTIME=podman`. Tenant lifecycle/status will use mt-admin,
not direct system-bus tenant service calls. Credentials are supplied from the
operator's existing environment and must not appear in this report or logs.
