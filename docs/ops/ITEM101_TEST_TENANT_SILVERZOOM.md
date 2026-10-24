# Item 101 — Test tenant `silverzoom` (kimi + pebble external workers, full toolchains)

> Provisioned 2026-10-24 by Rarity (autonomous), branch
> `aj/kimi-101-20261024-1200` (tenant clone at `722d07a47`).
> **This tenant is deliberately left running.** The name `silverzoom` is from
> the sanctioned test-tenant list in
> [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider) and is
> not an animal name, so the animal-name teardown routine must not target it.

## Purpose

Item 101 of `docs/ops/AGENT_GOALS_2.1.6.0.md`: stand up a test tenant with the
**kimi** and **pebble** external workers enabled with full toolchains, exercise
both workers with complex tasks, and report findings here. Sibling items 99
and 100 carry the same text and remain open; this tenant is item 101's.

## Host

Arch Linux cloud VM (**systemd** — tenant units are `systemctl --user` units on
the tenant's own manager, rootless podman 6.1.3, Quadlet containers), 12
threads, 23 GB RAM. WireGuard mesh (`wt0`) carries the shared TensorZero
upstream at `http://100.88.232.146:3000/openai/v1`. Existing tenants:
`antelope` (10000), `barracuda` (10010), `hermes` (10040); `silverzoom` took
the free block **10020-10029**.

## Provisioning record (add-tenant)

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh add-tenant silverzoom \
  --with-pebble --with-kimi --no-ssh \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --xmpp-jid silverzoom@xmpp.sobe.world
```

`ADD_EXIT=0` (`/tmp/add-tenant-101.log`).

- Port block **10020-10029**: gateway 10020, http 10021, bridge 10022,
  postgres 10023, proxy 10024 (disabled), weechat 10025, orchestrator 10026,
  pebble_wss 10028, weechat_adapter 10029; extended: `vision_service` 20025,
  `vision_health` 20026, `opencode_wss` 20027 (reserved), `coord_admin` 20029;
  kimi_wss 30020 / kimi_health 30021.
- Workers selected: `pebble`, `kimi` (opencode v1 and opencode2 intentionally
  not selected — this item is about kimi and pebble).
- `--llm-base-url` is the mesh upstream (not the unreachable 192.168.1.157 LAN
  address that item 74's runbook carried; see ITEM74 A1). WI#330 inheritance
  verified at provisioning time: `KIMI_BASE_URL` written with
  `KIMI_BASE_URL_SOURCE=inherited`.
- `--no-ssh` this time (the SSH harness is unrelated to worker dispatch and
  items 72/74 already cover it).

## Build record (build-tenant)

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh build-tenant silverzoom \
  --with-wasm --with-pebble --with-kimi --with-toolchains
```

_(filled in after the build completes)_

## Findings

_(filled in after worker exercise)_
