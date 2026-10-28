# Item 99 (v2.1.7.0) — Test Tenant `mistyfly` (kimi + pebble external workers, full toolchains)

> Provisioned 2026-10-28 by SweetieBot (autonomous), branch
> `sweetiebot/kimi-99-20261028-0605`, at tree `62bed7142` (LunarWing 2.1.7).
> The tenant clone tracks this branch: mt-admin's `clone_tenant_repo` clones
> the running checkout's current branch.
>
> **This tenant is deliberately left running.** `mistyfly` comes from the
> sanctioned test-tenant list in
> [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider) and is
> **not an animal name**, so the animal-name teardown routine must not target
> it.
>
> **Not a duplicate.** The v2.1.6.0 cycle's item 99 already stood up
> `thunderlane` — see
> [ITEM99_TEST_TENANT_THUNDERLANE.md](ITEM99_TEST_TENANT_THUNDERLANE.md) and
> [ITEM101_TEST_TENANT_SILVERZOOM.md](ITEM101_TEST_TENANT_SILVERZOOM.md).
> v2.1.7.0 items 99/100/101 carry the same text and are explicitly marked
> *multi-pass, multi-agent goal, not duplicate*, so this pass provisions a
> **fresh** tenant on a fresh port block and re-runs the worker exercise
> against the 2.1.7 tree (new daemon, new worker images, new mt-admin).

## Purpose

Item 99 of `docs/ops/AGENT_GOALS_2.1.7.0.md`: stand up a new test tenant with
the **kimi** and **pebble** external workers enabled with full toolchains,
assign both workers complex tasks, and report findings in a new doc under
`docs/ops`. Keep the tenant running afterwards.

## Host

Gentoo dev VM (OpenRC first-class), rootless podman 5.8.2, OpenRC 0.63.3,
kernel 6.18.54-gentoo, 16 cores, 31 GB RAM (+23 GB swap), 319 GB free disk on
`/home` and `/var`.

Pre-existing tenants: `antelope` (10000-10009), `scootaloo` (10010-10019),
`barracuda` (10020-10029), `thunderlane` (10030-10039). `mistyfly` takes the
next free block, **10040-10049**.

## Provisioning record (add-tenant)

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  bash ic/scripts/lunarwing-mt-admin.sh add-tenant mistyfly \
  --xmpp-jid mistyfly@xmpp.sobe.world \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-pebble --with-kimi --with-study-mcp
```

`ADD_EXIT=0`. Notes on the invocation:

- `--docker-group` was **omitted** (the prior passes passed it and got the
  cosmetic `group 'podman' does not exist; skipping` warning). This host runs
  rootless podman, so the opt-in group membership is not needed and the
  documented reason for it — reaching a shared rootful daemon socket — does
  not apply.
- `--llm-base-url` is the WireGuard-mesh TensorZero address every healthy
  tenant on this host uses (item-74 finding A1: the item-72 runbook's LAN
  `192.168.1.157` is unreachable from this host). With `--llm-base-url` given
  and no `--enable-proxy`, WI#330's "no built-in default" rule is satisfied
  without `--tensorzero-url`.
- Workers selected: **pebble + kimi** only — exactly the item's ask. opencode
  and opencode2 are intentionally not selected.
- `--with-study-mcp`: shared lw2studymcp checkout synced to
  `/var/cache/lunarwing/lw2studymcp`; tenant knowledge seed armed
  (`KNOWLEDGE_SEED_FROM_STUDY_MCP=1`, first boot only) plus bounded additive
  hourly refresh.

### Port block 10040-10049 (from `/etc/lunarwing/ports.json`)

| slot | port | | slot | port |
| --- | --- | --- | --- | --- |
| gateway | 10040 | | pebble_wss | 10048 |
| http | 10041 | | weechat_adapter | 10049 |
| bridge | 10042 | | pebble_health | 20044 |
| postgres | 10043 | | vision_service / health | 20045 / 20046 |
| proxy | 10044 | | kimi_wss | 30040 |
| weechat | 10045 | | kimi_health | 30041 |
| orchestrator | 10046 | | opencode2_wss (unused) | 51040 |

### WI#330 worker URL inheritance

`KIMI_BASE_URL` was written as the tenant's own LLM endpoint with
`KIMI_BASE_URL_SOURCE=inherited` — the container-reachable endpoint is
inherited rather than defaulted, exactly per the WI#330 design.

### Provisioning observations

- **P1 (host, pre-existing, same as items 72/74/99):** WeeChat relay
  auto-bootstrap failed (`generated WeeChat relay config failed validation`).
  Host WeeChat 4.10.0 lacks cJSON, so `/relay add api` cannot succeed. The
  tenant's weechat service still starts; only its relay is unconfigured.
  Recovery path: `configure-weechat-relay mistyfly`.
- **P2 (cosmetic, pre-existing):** `WARNING: cronie has already been started`
  from the health-pipeline install step.
- Pre-delegated cgroup v2 subtree now covers
  `antelope barracuda mistyfly scootaloo thunderlane`
  (`/etc/local.d/lunarwing-cgroups.start`).
- Sandbox image `lunarwing-sandbox:latest` distributed into mistyfly's rootless
  store via save|load.

## Build record (build-tenant)

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh build-tenant mistyfly \
  --with-wasm --with-pebble --with-kimi --with-toolchains
```

_(in progress — artifacts recorded below once `BUILD_EXIT` is known)_
