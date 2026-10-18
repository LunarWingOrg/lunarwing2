# Item 74 — Temporary Test Tenant `fleetfoot` (v2.1.6.0 landed-changes validation)

> Provisioned 2026-10-18 by Rarity (autonomous), branch
> `aj/kimi-74-20261018-0800`. mt-admin's `clone_tenant_repo` clones the
> running checkout's current branch, so the tenant runs exactly this tree
> (all goal-document changes landed through 2026-10-18, incl. items 63/65).
>
> **This tenant is deliberately left running.** The name `fleetfoot` comes
> from the sanctioned test-tenant list in
> [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider —
> re-fetched from the tracker 2026-10-18) and is not an animal name, so the
> animal-name teardown routine must not target it.

## Purpose

Item 74 of `docs/ops/AGENT_GOALS_2.1.6.0.md`: stand up a temporary test
tenant with the **kimicode**, **opencode2**, and **pebble** external workers
enabled with full toolchains, exercise the changes that landed from the goal
document, and report. Sibling items 72 (done, tenant `soarin` on the Gentoo
host) and 73 repeat the same goal.

## Host

Arch Linux dev VM (**systemd** — user-manager tenant units, rootless podman
6.1.3), 12 threads, 23 GB RAM, ~129 GB free disk at provisioning time.
Pre-existing tenants on the host: `antelope` (10000), `barracuda` (10010),
`spitfire` (10020), `hermes` (10040). `fleetfoot` took the next free block,
**10030-10039**. This is the first item-7x test tenant on a **systemd** host
in this series (item 72 ran on Gentoo/OpenRC), so the systemd user-unit +
Quadlet paths are the ones under test here.

## Provisioning record (add-tenant)

Command:

```bash
sudo LUNARWING_SERVICE_MANAGER=systemd LUNARWING_CONTAINER_RUNTIME=podman \
  bash ic/scripts/lunarwing-mt-admin.sh add-tenant fleetfoot \
  --docker-group \
  --xmpp-jid fleetfoot@xmpp.sobe.world \
  --llm-base-url http://192.168.1.157:3000/openai/v1 \
  --with-pebble --with-kimi --with-opencode2 --with-study-mcp
```

(`ADD_EXIT=0`; log `/tmp/add-tenant-74.log` during the run.)

- `--llm-base-url` points at the same upstream TensorZero the existing
  `hermes` tenant dials (WI#330: no built-in default anymore).
- Port block **10030-10039**: gateway 10030, http 10031, bridge 10032,
  postgres 10033, proxy 10034 (disabled), weechat 10035, orchestrator 10036,
  pebble_wss 10038, weechat_adapter 10039; extended: opencode_wss 20037,
  vision 20035/20036, coord_admin 20039; kimi_wss 30030; opencode2_wss 51030.
- Workers selected: `pebble`, `kimi`, `opencode2` (opencode v1 intentionally
  not selected — item 2 froze v1).
- `--with-study-mcp`: shared lw2studymcp checkout synced; tenant knowledge
  seed armed (`KNOWLEDGE_SEED_FROM_STUDY_MCP=1`, first boot only).
- Tenant rustup 1.99.1 + `wasm32-wasip2` target installed into the tenant
  account (WASM lane prerequisite).
- Sandbox image `lunarwing-sandbox:latest` present/ready.
- WI#330 re-verified at provisioning time: `KIMI_BASE_URL` and
  `OPENCODE2_BASE_URL` written as the tenant's own LLM endpoint with
  `*_SOURCE=inherited` markers; the LAN address is container-reachable so
  workers inherit it.
- PostgreSQL container `lunarwing-pg-fleetfoot` on 10033 (rootless Quadlet).

## Build record (build-tenant)

_IN PROGRESS at skeleton commit — filled in as the run completes._

## Runtime verification

_Pending._

## Landed-changes exercise matrix

_Pending._

## Job dispatch end-to-end

_Pending._

## Anomalies and follow-ups

_Pending._

## Disposition

The tenant is **left running** (all services boot-persistent under the
systemd user manager: `systemctl --user enable` + linger) for maintainer
inspection:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh status fleetfoot
uid="$(id -u fleetfoot)"
sudo -u fleetfoot XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user status lunarwing-fleetfoot.service
```

It is not an animal name and comes from the WI#273 list, so the teardown
routine must not target it. To decommission later anyway:
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant fleetfoot --purge`.
