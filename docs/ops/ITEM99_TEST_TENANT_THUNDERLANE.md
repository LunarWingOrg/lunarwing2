# Item 99 — Temporary Test Tenant `thunderlane` (kimi + pebble external workers, full toolchains)

> Provisioned 2026-10-24 by SweetieBot (autonomous), branch
> `sweetiebot/kimi-99-20261024-1205`. The tenant runs exactly this tree:
> mt-admin's `clone_tenant_repo` clones the running checkout's current branch.
>
> **This tenant is deliberately left running.** The name `thunderlane` comes
> from the sanctioned test-tenant list in
> [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider) and is
> not an animal name, so the animal-name teardown routine must not target it.

## Purpose

Item 99 of `docs/ops/AGENT_GOALS_2.1.6.0.md`: stand up a new test tenant with
the **kimi** and **pebble** external workers enabled with full toolchains,
assign both workers complex tasks, and report findings. Sibling items 100/101
repeat the same goal for later agents.

## Host

Gentoo dev VM (OpenRC first-class), rootless podman, 31 GB RAM, 325 GB free
disk. Pre-existing tenants: `antelope` (10000-10009), `scootaloo`
(10010-10019), `barracuda` (10020-10029). `thunderlane` takes the next free
block, **10030-10039**.

## Provisioning record (add-tenant)

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  bash ic/scripts/lunarwing-mt-admin.sh add-tenant thunderlane \
  --docker-group \
  --xmpp-jid thunderlane@xmpp.sobe.world \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-pebble --with-kimi --with-study-mcp
```

- `--llm-base-url` is the WireGuard-mesh TensorZero address every healthy
  tenant on this host uses (item 74 finding A1: the item-72 runbook's LAN
  `192.168.1.157` is unreachable from this host).
- Port block **10030-10039**: gateway 10030, http 10031, bridge 10032,
  postgres 10033, proxy 10034, weechat 10035, orchestrator 10036,
  pebble_wss 10038, opencode_wss 20037, opencode2_wss 51030,
  kimi_wss 30030, weechat_adapter 10039.
- Workers selected: `pebble` + `kimi` (exactly the item's ask; opencode and
  opencode2 intentionally not selected).
- `--with-study-mcp`: shared lw2studymcp checkout synced to
  `/var/cache/lunarwing/lw2studymcp`, published generation
  `5857c69e81b25f22e3de30a2f6ff33f019fc1c42`, tenant knowledge seed armed
  (`KNOWLEDGE_SEED_FROM_STUDY_MCP=1`, first boot only).
- PostgreSQL 16 (pgvector image) as `lunarwing-pg-thunderlane` on 10033.
- OpenRC init scripts + conf.d rendered; pre-delegated cgroup v2 subtree now
  covers `antelope barracuda scootaloo thunderlane`.
- Sandbox image `lunarwing-sandbox:latest` distributed into thunderlane's
  rootless store.

### WI#330 worker URL inheritance

`KIMI_BASE_URL` was written as the tenant's own LLM endpoint with
`KIMI_BASE_URL_SOURCE=inherited` — the container-reachable endpoint is
inherited rather than defaulted, exactly per the WI#330 design.

### Provisioning anomalies

- **P1 (host, pre-existing, same as item 72 A2):** WeeChat relay
  auto-bootstrap failed (`generated WeeChat relay config failed validation`).
  The tenant's weechat service starts but its relay is not configured.
  Host WeeChat 4.10.0 lacks cJSON, so `/relay add api` cannot succeed.
  Recovery path documented by mt-admin itself:
  `configure-weechat-relay thunderlane`.
- **P2 (cosmetic):** `group 'podman' does not exist; skipping` for
  `--docker-group` — expected on this rootless-podman host.
- **P3 (cosmetic):** `Creating mailbox file: No such file or directory` from
  `useradd` — the tenant user is created correctly; the host has no
  `/var/spool/mail`.

## Build record (build-tenant)

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh build-tenant thunderlane \
  --with-wasm --with-pebble --with-kimi --with-toolchains
```

(Filled in below — see the verification section.)

## Verification

`start-tenant thunderlane` → `START_EXIT=0`. All services under OpenRC with
`-sup` crash supervisors, boot-persistent (default runlevel):

- PostgreSQL (`lunarwing-pg-thunderlane`, 10033), daemon
  (`lunarwing-thunderlane`), xmpp-bridge, weechat + WS adapter, LunarVision
  sidecar (healthy, 20035/20036), and both worker containers
  (`lunarwing-pebble-thunderlane` :10038, `lunarwing-kimi-thunderlane` :30030).
- Gateway `:10030/api/health` → `{"status":"healthy","channel":"gateway"}`.
- `/api/gateway/status` (bearer): version 2.1.6, `llm_backend`
  `openai_compatible`, model `tensorzero::function_name::lunarwing`, channels
  unix_socket_repl/multica/weechat/xmpp/http/gateway, skills enabled.
  `multica` reports unhealthy (no multica server configured — cosmetic, same
  as every prior test tenant).
- **13 extensions active**: `study_notes` MCP + 9 WASM tools + 3 WASM
  channels (`multica`, `weechat`, `xmpp`).
- **WI#357 exposure audit**: `worker-listeners thunderlane` → `clean`. Both
  workers publish host loopback only (10038 / 30030 plus the in-container
  health 8443), no self-exposed inbound path.
- Worker bridges live: kimi `[bridge] server listening on
  ws://0.0.0.0:30030/ws/agent`, subprotocol `lunarwing-agent-v1`, auth
  enabled; pebble `[bridge] listening on ws://0.0.0.0:10038/ws/agent`.
  The kimi task runner starts as the task uid via the root bridge
  (WI#400 shape).
- **Agent LLM loop**: first gateway chat turn answered `BOOT_OK` in 41 s
  (`state: Completed`), proving the tenant's own LLM path
  (`LLM_BASE_URL=http://100.88.232.146:3000/openai/v1`) is live.
- LLM endpoint independently probed from the host with the tenant's
  `LLM_API_KEY` → HTTP 200 for both `tensorzero::function_name::lunarwing`
  and `tensorzero::function_name::FrontierCODEK` (the kimi lane's model).

## Worker task exercise

(Filled in below.)

## Findings

(Filled in below.)

## Disposition

The tenant is **left running** (all services boot-persistent under OpenRC)
for maintainer inspection:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh status thunderlane
sudo -u thunderlane bash -c 'XDG_RUNTIME_DIR=/run/user/$(id -u thunderlane) podman ps'
```

It is not an animal name and comes from the WI#273 list, so the teardown
routine must not target it. To decommission later anyway:
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant thunderlane --purge`.
