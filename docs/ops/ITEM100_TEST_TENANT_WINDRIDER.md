# Item 100 — persistent test tenant `windrider`

## Scope and status

This is the v2.1.7.0 item-100 pass on branch
`rarity/kimi-100-20261029-1200`, started 2026-10-29 by Rarity.
The deliverable is a new tenant with kimi and pebble external workers,
full toolchains, complex tasks executed through the real worker dispatch path,
and independently checked results. The tenant must remain running.

**In progress: provisioning and runtime evidence have not yet been collected.**
No successful build, worker task, or completion is claimed by this initial record.

`windrider` is a non-animal name from the project's WI#273 test-tenant list.
It must remain available for inspection and must not be targeted by the routine
animal-name teardown. This pass does not reuse item 99's tenant or results.

## Host and procedure

- Operator checkout: `/home/rarity/lunarwing2_gitlab` on `colgatev`.
- Host: Linux/OpenRC, podman available, 18 CPUs, approximately 95 GB free
  on the shared `/home` and `/var` filesystem before provisioning.
- Existing registry contains `dash`; existing tenants are not part of this pass.
- Use `ic/scripts/lunarwing-mt-admin.sh` for provisioning and lifecycle.
- Build full worker toolchains with `build-tenant --with-pebble --with-kimi
  --with-toolchains`; run long commands in tmux and bound builds to 18 CPUs.
- Credentials come from the existing process environment and are passed to
  supported configuration commands without printing them or committing them.
- Record actual image provenance, ports, job IDs, verification results,
  operational failures, and final live health as they become available.
