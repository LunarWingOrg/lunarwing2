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

`start-tenant soarin` brought up, via OpenRC + `-sup` crash supervisors:
PostgreSQL, daemon, xmpp-bridge, weechat + WS adapter, LunarVision sidecar
(healthy, ports 20005/20006), and the three worker containers
(`lunarwing-pebble-soarin` :10008, `lunarwing-opencode2-soarin` :51000,
`lunarwing-kimi-soarin` :30000), each health-monitored and boot-persistent.
SSH harness: key uploaded to the secrets store, staged key deleted, agent
bridge `running` with `keys_loaded=1` (`/agent/status` on the HTTP port with
the `SSH_API_TOKEN` bearer).

- Gateway `:10000/api/health` → `{"status":"healthy"}` (no auth); API surface
  with the `GATEWAY_AUTH_TOKEN` bearer works (`/api/gateway/status`: version
  2.1.6, llm_backend `openai_compatible`, model
  `tensorzero::function_name::lunarwing`, enabled channels
  unix_socket_repl / multica / weechat / xmpp / http / gateway, skills on).
- **Study Notes MCP** (provisioned via `--with-study-mcp`): extension
  `study_notes` active, authenticated, read-only, tools exposed.
- **Knowledge seed** (first boot, one-shot): 1796 notes seeded, 25 skipped
  (already present), 20 failed (recorded in
  `state/knowledge_seed_state.json`; failures are import-level warnings —
  non-lowercase tags and one transient `db error`), seed marked complete and
  will not rerun. The landed **knowledge sweep** then ran its first pass:
  1817 notes scanned, 781 marked stale per the 30-day rule, 0 stranded
  removed.
- **WI#357 exposure audit**: `mt-admin worker-listeners soarin` →
  `worker listener audit: clean` — all three workers publish host-loopback
  only; no self-exposed inbound path.
- **WI#380 / item 44**: production `handle_message_timeout` default is 630 s
  (`ic/src/settings.rs:723`, `default_handle_message_timeout()`); the
  per-tenant settings store is empty so the code default applies.
- **Host-global health pipeline** (items 22/51 lane): `lunarwing-mt-health`
  runs clean — gateway/xmpp/omemo/ratelimit/clickhouse/tensorzero/models/
  llm_streams/lunarvision/lunarvoice/netbird/knowledge/**openrc** all
  healthy (the WI#410 OpenRC safe-unit contract check passes through the
  shared library); `runtime` reports degraded only because soarin's XMPP
  bridge cannot authenticate upstream (see A3).

## Landed-changes exercise matrix

| Landed change (goal item) | Evidence on soarin |
| --- | --- |
| WI#330 no-default LLM endpoint + worker inheritance (10) | `KIMI_BASE_URL`/`OPENCODE2_BASE_URL` written with `*_SOURCE=inherited` from the tenant endpoint |
| kimi worker @ kimi-code 2.2.0 (4, WI#324) | image pin verified (`/opt/kimi-cli/package.json` → 2.2.0); real job ran on the lane |
| kimi `max_output_size=131072` (53, WI#412) | `config/config.toml` in-image pin verified |
| WI#411 `KIMI_CODE_TRUST_WORKSPACE` scrub (52) | both entrypoint modes `unset` it; executor deletes it post-merge |
| opencode2 lane (2: frozen v1, v2 selected) | `--with-opencode2` provisioned, fat image, job completed on the lane |
| pebble worker lane | container + dispatch + exec verified; LLM key is the operator's to add (`configure-pebble --nanogpt-api-key`) |
| WI#357 workers never self-expose (21) | `worker-listeners soarin` → clean |
| Approval gate / pending state | `create_job` correctly parked the turn at the `approval` gate; `action=always` resumed it (SSE `gate_required`/`approval_needed` events observed) |
| Knowledge seed + sweep + study MCP (5/6 lane) | 1796 seeded; sweep pass ran; study_notes extension active |
| WASM toolchain lanes (channels/tools) | 3 channels + 9 tools installed and loaded (channels listed in gateway status) |
| Health/self-heal pipeline (22, 51/WI#410) | fleet check green incl. openrc component; cronie scheduler repaired on the host |
| Engine v2 turn lifecycle | turns transition Created→Running→Completed→Done; stream_chunks + tool events on SSE |

## Job dispatch end-to-end (the core of the item)

Driven through the real path (gateway chat → LLM loop → `create_job` →
orchestrator on-demand WS dial → worker bridge → container executor →
job_events → assistant reply):

- **kimi** — job `b9fa328d-1cf3-46f2-b7dc-4828c134b28d`: bridge log
  `agent connected (1 active)` → `task …: starting` → `success (6278ms)`;
  `job_events`: `message {"content":"KIMI_WORKER_OK"}`,
  `result {"content":"Completed: KIMI_WORKER_OK"}`; assistant reported the
  job id. **This is the WI#324 real-worker evidence the maintainer's
  2026-10-15 rebuild note asks for — on a tenant running an image built from
  a tree far past `766b7c91e`.**
- **opencode2** — job `b1c29961-0ce8-4655-a6f4-3e118528dfa4`: bridge
  `success (9559ms)`; result `Completed: OC2_WORKER_OK`.
- **pebble** — job `fc84b138-2564-48ff-bcbc-9b99ead67b17`: dispatch, exec
  spawn, and result forwarding all worked; the job itself failed with
  `required API key is not configured` — the pebble worker's nanogpt
  credential is unset. scootaloo's pebble is in the same state, so this is
  the standing host condition, not a soarin regression.

Operational note discovered while driving this: the v1
`/api/chat/history` view reports an in-flight engine-v2 turn as
`"state":"Failed"` with an empty response while the engine actually has it
`Running` (confirmed via the SSE `thread_state_changed` stream, where the
same thread later emits `Completed→Done`). The UI/history shim mislabeling
pending turns as Failed is worth a follow-up work item; the engine itself
is correct.

## Anomalies and follow-ups

- **A1 (fixed in-tree by this item):** noexec `/tmp` (tmpfs) killed the
  tenant rustup install inside `ensure_tenant_wasm_toolchain`
  (`Cannot execute …/rustup-init`), silently skipping the WASM lane. Fixed
  by staging rustup-init under `$HOME/tmp`
  (`ic/scripts/mt-admin-lib/05-tenant-build.sh`, commit on this branch);
  soarin's WASM suite was then built and installed (12 extensions).
- **A2 (host, pre-existing, both tenants):** host WeeChat 4.10.0 lacks
  cJSON, so `/relay add api` fails (`unable to add relay "api" (cJSON
  support…)`) and both tenants' weechat WS adapters are degraded. Fix is
  host-side (rebuild weechat with json support); additionally
  `configure-weechat-relay` refuses to recover once the weechat service has
  created its config dir, so the documented tmux manual path was used to
  set the relay password.
- **A3 (operator credential):** `soarin@xmpp.sobe.world` XMPP auth fails
  (`NotAuthorized`) — the account does not exist on the server; in-band
  registration did not happen. Needs the XMPP server operator to create
  the account (same manual step scootaloo needed).
- **A4 (operator credential):** pebble worker needs
  `configure-pebble soarin --nanogpt-api-key <key>` before its jobs can
  call an LLM (lane verified without it).
- **A5 (host, fixed on the spot):** `/var/spool/cron/crontabs` was missing
  and cronie was down — the health-pipeline crontab could not install.
  Repaired (`mkdir -p` + `rc-service cronie start`); pipeline now on its
  5-minute cadence.
- **A6 (cosmetic):** multica channel fails to start (no multica server
  configured for this tenant) and the first-boot window logged
  connection-refused against the weechat relay before A2 was known — both
  expected for an unconfigured test tenant.
- **A7 (follow-up candidate):** `/api/chat/history` mislabels pending
  engine-v2 turns as `Failed` (see operational note above).

## Disposition

The tenant is **left running** (all services boot-persistent under OpenRC)
for maintainer inspection:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh status soarin
sudo -u soarin bash -c 'XDG_RUNTIME_DIR=/run/user/$(id -u soarin) podman ps'
```

It is not an animal name and comes from the WI#273 list, so the teardown
routine must not target it. To decommission later anyway:
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant soarin --purge`.
