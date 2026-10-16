# Item 72 — Temporary Test Tenant `soarin` (v2.1.6.0 landed-changes validation)

> Provisioned 2026-10-16 by SweetieBot (autonomous), branch
> `sweetiebot/kimi-72-20261016-2205` @ `962c08065` (= `origin/master` `6fc920117`
> + goals-doc-only commits). The tenant runs exactly this tree: mt-admin's
> `clone_tenant_repo` clones the running checkout's current branch.
>
> **This tenant is deliberately left running.** The name `soarin` comes from the
> sanctioned test-tenant list in [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider) and is not
> an animal name, so the animal-name teardown routine must not target it.

## Purpose

Item 72 of `docs/ops/AGENT_GOALS_2.1.6.0.md`: stand up a temporary test tenant
with the **kimicode**, **opencode2**, and **pebble** external workers enabled
with full toolchains, exercise the changes that landed from the goal document,
and report. Sibling items 73/74 repeat the same goal for later agents.

## Host

Gentoo dev VM (OpenRC first-class), rootless podman 5.8.2, 31 GB RAM, 361 GB
free disk. Pre-existing tenant on the host: `scootaloo` (port block 10010-10019,
all four workers, live since 2026-10-10). `soarin` coexists on the next free
block.

## Provisioning record (add-tenant)

Command:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh add-tenant soarin \
  --docker-group \
  --xmpp-jid soarin@xmpp.sobe.world \
  --llm-base-url http://192.168.1.157:3000/openai/v1 \
  --with-pebble --with-kimi --with-opencode2 --with-study-mcp
```

- `--llm-base-url` points at the same upstream TensorZero the existing
  `scootaloo` tenant dials (WI#330: there is no built-in default anymore).
- Port block **10000-10009**: gateway 10000, http 10001, bridge 10002,
  postgres 10003, proxy 10004, weechat 10005, orchestrator 10006,
  pebble_wss 10008, opencode_wss 20007, opencode2_wss 51000, kimi_wss 30000,
  weechat_adapter 10009.
- Workers selected: `pebble`, `kimi`, `opencode2` (opencode v1 intentionally
  not selected — the v2 lane replaces it; item 2 froze v1 updates).
- `--with-study-mcp`: shared lw2studymcp checkout synced to
  `/var/cache/lunarwing/lw2studymcp`, tenant knowledge seed armed
  (`KNOWLEDGE_SEED_FROM_STUDY_MCP=1`, first boot only).
- PostgreSQL 16 (pgvector image) running as `lunarwing-pg-soarin` on 10003.
- OpenRC init scripts + conf.d rendered for all tenant services; pre-delegated
  cgroup v2 subtree now covers `scootaloo` and `soarin`
  (`/etc/local.d/lunarwing-cgroups.start`).
- Sandbox image `lunarwing-sandbox:latest` built and distributed into soarin's
  rootless store.
- WI#330 verification (env inheritance, first landed change re-verified live):
  `KIMI_BASE_URL` and `OPENCODE2_BASE_URL` were written as the tenant's own LLM
  endpoint with `*_SOURCE=inherited` markers; the LAN address is
  container-reachable so workers inherit it, exactly per the WI#330 design.

### Host repair during provisioning (pre-existing, affects all tenants)

The host-global health pipeline install warned
`failed to install crontab schedule` because `/var/spool/cron/crontabs` did not
exist and `cronie` was not running (`rc-service cronie start` → `[ !! ]`).
Repaired on the spot:

```bash
sudo mkdir -p /var/spool/cron/crontabs && sudo rc-service cronie start   # -> [ ok ]
```

`rc-update` already had cronie in runlevel `default`; the spool dir was simply
missing on this host. The health pipeline then reported
"ready (every 5 min; covers all tenants)".

## Build record (build-tenant)

First pass (`--with-wasm --with-pebble --with-kimi --with-opencode2
--with-toolchains`, tmux, `BUILD_EXIT=0`):

- Daemon release binary `ic/target/release/lunarwing` (110 MB) + xmpp-bridge
  built from the tenant clone (branch `sweetiebot/kimi-72-20261016-2205`).
- Worker images, all **fat (Rust/Go/C++ toolchains)**, in the admin (root)
  podman store:
  - `lunarwing-worker-pebble:latest` — 2.33 GB (source revision `7a0bff1af`)
  - `lunarwing-worker-kimi:latest` — 2.68 GB (source revision `962c08065`)
  - `lunarwing-worker-opencode2:latest` — 2.9 GB (source revision `962c08065`)
  (pebble builds from the live checkout, kimi/opencode2 from the tenant clone —
  both are the same worker-source tree; the branch's extra commits touch only
  docs and mt-admin.)
- **WASM lane skipped on the first pass** — see anomaly A1 (noexec `/tmp`
  killed the tenant rustup install). Fixed, then a second
  `build-tenant soarin --with-wasm` pass completed `WASM_EXIT=0`:
  - channels: `multica`, `weechat`, `xmpp` installed (`darkirc` correctly
    skipped — tenant darkirc disabled)
  - tools: `cavepony-tool`, `gitlab-tool`, `gotify-tool`, `llm-context-tool`,
    `multica-bridge-tool`, `ssh-tool`, `vision-analyze-tool`,
    `voice-transcribe-tool`, `web-search-tool`
  - weechat caps patched to the tenant's relay/adapter ports; ssh-tool
    allowlist `["127.0.0.1"]`

### In-image pins verified (items 4, 52, 53 — WI#324/WI#411/WI#412)

Against the freshly built `lunarwing-worker-kimi:latest`:

- `@moonshot-ai/kimi-code` pinned at **2.2.0** (`/opt/kimi-cli/package.json`).
- `config/config.toml` pins **`max_output_size = 131072`** on the
  FrontierCODEK model (with the rationale comment) — the config-side AC of the
  maintainer's rebuild note; this image was built from a tree far past
  `766b7c91e`.
- The WI#411 scrub is present: both `entrypoint.sh` modes
  `unset KIMI_CODE_DANGEROUS_COMMAND_GUARD KIMI_CODE_TRUST_WORKSPACE`, and
  `kimi_task_executor.ts` deletes `KIMI_CODE_TRUST_WORKSPACE` after every env
  merge.

## Runtime verification

(pending build + start)

## Landed-changes exercise matrix

(pending)

## Job dispatch end-to-end

(pending)

## Anomalies and follow-ups

(pending)
