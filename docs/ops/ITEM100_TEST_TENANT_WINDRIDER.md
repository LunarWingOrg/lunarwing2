# Item 100 (v2.1.7.0) — persistent test tenant `windrider`

## Scope and status

This is the v2.1.7.0 item-100 pass on branch
`rarity/kimi-100-20261029-1936`, continuing the earlier
`rarity/kimi-100-20261029-1200` pass (merged as `98118fdad`).
Started 2026-10-29 by Rarity. No subagents are used.

The deliverable is a **new** tenant with kimi and pebble external workers,
full toolchains, complex tasks executed through the real worker dispatch path,
and independently checked results. The tenant must remain running afterwards
and must not be torn down.

`windrider` is a non-animal name from the project's
[WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
test-tenant list, so it is not matched by the routine animal-name teardown.
This pass does not reuse item 99's tenants (`mistyfly`, `thunderlane`) or
item 101's (`highwinds`, `silverzoom`) or their results.

## What the earlier 1200 pass landed

That pass provisioned `windrider` and fixed the **rootless egress keeper**
blocker that stopped item 101 on a Podman 6.1.4 host. On this host podman is
5.8.2 and the built-in infra container is image-less: `ImageName == ""`,
`IsInfra=true`, `Path=/catatonit`, `Args=["-P"]`, local generated rootfs.
The pre-existing guard accepted only `localhost/podman-pause:<tag>`, so every
standard `start-worker` failed closed before it could start its replacement —
exactly the item-101 hand-off failure.

Commits already in this branch's history:

- `c7438ad75` identify Podman native pause keeper metadata
- `fb502652d` accept constrained image-less Podman namespace keepers
- `72f589f6d` model native pause metadata in egress fixtures
- `1dc0a3838` cover native keeper acceptance and hostile metadata

`ic/scripts/lunarwing-worker-egress-guard.sh` now falls back to
`keeper_uses_native_pause()` when `ImageName` is empty. That fallback is
**shape-pinned, not trust-based**: it requires the exact tuple
`""|/catatonit|["-P"]|[]|/run/user/<uid>/libpod/tmp/infra-container`,
including an empty mount list and the per-uid generated rootfs path, so an
arbitrary image-less or mounted-rootfs container is still removed and the
start refused. A non-empty `ImageName` still has to match
`localhost/podman-pause:*`. The label, container-count, `IsInfra` and
exclusive-network checks are unchanged.

What the 1200 pass did **not** do: build worker images, start the workers, or
run any complex task. Its record explicitly claimed no build, no worker task
and no completion. This pass is that work.

## Host and procedure

- Operator checkout: `/home/rarity/lunarwing2_gitlab` on the dev VM.
- Host: Linux/OpenRC, rootless podman **5.8.2**, 18 CPUs, 17 GiB RAM,
  ~85 GB free on the shared `/` (which holds `/home`, `/var` and `/tmp`)
  before provisioning.
- Tenant uid **1004**; allocated block base_port 10010 — gateway 10010,
  http 10011, postgres 10013, orchestrator 10016, pebble_wss 10018,
  kimi_wss 30010, kimi_health 30011, pebble_health 20004,
  vision_service/health 20015/20016.
- Pre-existing state found at the start of this pass: tenant user, port
  allocation, cloned checkout and generated env all present; PostgreSQL
  container running; daemon release binary already built
  (`/home/windrider/lunarwing/ic/target/release/lunarwing`, 111 MB);
  **no** WASM extensions installed, **no** worker images, daemon stopped.
- All lifecycle operations go through `ic/scripts/lunarwing-mt-admin.sh`
  (root). No bare `systemctl`/`rc-service` against tenant units from
  wrappers, per the AGENTS.md multi-tenant hard rule.
- Long builds run in tmux, bounded to 18 CPUs. No full debug daemon build is
  used.
- Credentials come from the existing process environment and are passed to
  supported configuration commands without printing or committing them.
- Logs are kept under `/tmp/lw-item100/` on this host.

## Plan

1. Start the tenant daemon and confirm the gateway answers.
2. Build the kimi and pebble worker images with `--with-toolchains`, then
   install WASM tools into the tenant state dir.
3. Start both workers through mt-admin, configure and audit the per-tenant
   egress allowlist and listener exposure.
4. Dispatch complex tasks to each worker through the real gateway dispatch
   path (`create_job`), one lane at a time, into distinct workspace
   subdirectories.
5. Verify each artifact against an **independent** oracle on the host — never
   by re-asking the worker that produced it.
6. Record findings, image provenance, ports, job IDs and final live health
   here. Leave the tenant running; no teardown.

No secret inspection, no unrelated project changes, no subagents.
