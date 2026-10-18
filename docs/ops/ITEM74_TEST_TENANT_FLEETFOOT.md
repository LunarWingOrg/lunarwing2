# Item 74 — Temporary Test Tenant `fleetfoot` (v2.1.6.0 landed-changes validation)

> Provisioned 2026-10-18 by Rarity (autonomous), branch
> `aj/kimi-74-20261018-0800` (HEAD `f3ab996cc`; the tenant clone sits at
> `6b2bfe59a` = master head — mt-admin's `clone_tenant_repo` clones the
> running checkout's current branch at add-tenant time, and the only later
> commit on this branch is this report's docs-only skeleton). The tenant
> therefore runs every goal-document change landed through 2026-10-18,
> including items 63 (WI#364) and 65 (WI#371).
>
> **This tenant is deliberately left running.** The name `fleetfoot` comes
> from the sanctioned test-tenant list in
> [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider —
> re-fetched from the tracker 2026-10-18; `spitfire` already in use here)
> and is not an animal name, so the animal-name teardown routine must not
> target it.

## Purpose

Item 74 of `docs/ops/AGENT_GOALS_2.1.6.0.md`: stand up a temporary test
tenant with the **kimicode**, **opencode2**, and **pebble** external workers
enabled with full toolchains, exercise the changes that landed from the goal
document, and report. Sibling items: 72 (done 2026-10-16, tenant `soarin`
on the Gentoo/OpenRC host) and 73 (open).

## Host

Arch Linux cloud VM (**systemd** — tenant units are `systemctl --user` units
on the tenant's own manager, rootless podman 6.1.3, Quadlet containers),
12 threads, 23 GB RAM, ~129 GB free disk at provisioning time. Public
uplink on `enp1s0`; a WireGuard mesh (`wt0`, 100.88.0.0/16) carries the
shared TensorZero upstream — **there is no 192.168.1.0/24 LAN on this host**
(see anomaly A1). Pre-existing tenants: `antelope` (10000),
`barracuda` (10010), `spitfire` (10020), `hermes` (10040); `fleetfoot` took
the next free block, **10030-10039**. This is the first item-7x test tenant
on a **systemd** host (item 72 covered Gentoo/OpenRC), so the systemd
user-unit + Quadlet paths are the ones under test here.

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

(`ADD_EXIT=0`; run log was `/tmp/add-tenant-74.log`.)

- The `--llm-base-url` value above was the standing value from the item-72
  runbook (the Gentoo host's LAN). On this host it is unreachable; it was
  corrected post-provision to the mesh upstream — see anomaly A1. The
  correction is also what `configure-kimi` / `configure-opencode2`
  re-pointed for the workers.
- Port block **10030-10039**: gateway 10030, http 10031, bridge 10032,
  postgres 10033, proxy 10034 (disabled), weechat 10035, orchestrator 10036,
  pebble_wss 10038, weechat_adapter 10039; extended: opencode_wss 20037
  (reserved), vision 20035/20036, coord_admin 20039; kimi_wss 30030;
  opencode2_wss 51030.
- Workers selected: `pebble`, `kimi`, `opencode2` (opencode v1 intentionally
  not selected — item 2 froze v1).
- `--with-study-mcp`: shared lw2studymcp checkout synced to
  `/var/cache/lunarwing/lw2studymcp`; tenant knowledge seed armed
  (`KNOWLEDGE_SEED_FROM_STUDY_MCP=1`, first boot only).
- Tenant rustup 1.99.1 + `wasm32-wasip2` installed into the tenant account
  (WASM lane prerequisite; the item-72 noexec-`/tmp` fix is in this tree and
  the lane ran clean here).
- Sandbox image `lunarwing-sandbox:latest` ready.
- WI#330 re-verified at provisioning time: `KIMI_BASE_URL` and
  `OPENCODE2_BASE_URL` were written with `*_SOURCE=inherited` markers
  resolved from the tenant endpoint.
- PostgreSQL container `lunarwing-pg-fleetfoot` on 10033 (rootless Quadlet,
  health-gated).
- Boot persistence: `systemctl --user enable` symlinks rendered under
  `~fleetfoot/.config/systemd/user/default.target.wants/` +
  `loginctl enable-linger fleetfoot`.

## Build record (build-tenant)

One pass, `BUILD_EXIT=0` (tmux, log was `/tmp/build-tenant-74.log`):

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh build-tenant fleetfoot \
  --with-wasm --with-pebble --with-kimi --with-opencode2 --with-toolchains
```

- Daemon release binary `ic/target/release/lunarwing` (110 MB) + xmpp-bridge
  built from the tenant clone.
- **WASM lane: 12 installed, 1 skipped** — channels `multica`, `weechat`,
  `xmpp` (`darkirc` correctly skipped: tenant darkirc disabled); tools
  `cavepony-tool`, `gitlab-tool`, `gotify-tool`, `llm-context-tool`,
  `multica-bridge-tool`, `ssh-tool`, `vision-analyze-tool`,
  `voice-transcribe-tool`, `web-search-tool`. Weechat caps patched to the
  tenant's relay/adapter ports (10035/10039) at start.
- Worker images, all **fat (Rust/Go/C++ toolchains)**:
  - `lunarwing-worker-pebble:latest` — 2.33 GB (source revision `f3ab996cc`,
    built from the live checkout)
  - `lunarwing-worker-kimi:latest` — 2.68 GB (revision `6b2bfe59a`, tenant
    clone)
  - `lunarwing-worker-opencode2:latest` — 2.9 GB (revision `6b2bfe59a`;
    in-image self-tests ran during the build, including
    `exposure_policy_assert_test.py` and `compile_out_remote_tunnel_test.py`
    — the WI#357 build-time controls pass on the fat lane)
  (pebble builds from the live checkout, kimi/opencode2 from the tenant
  clone; the rev delta is only this report's docs-only skeleton commit.)

### In-image pins verified (items 4, 52, 53 — WI#324/WI#411/WI#412)

Against the freshly built `lunarwing-worker-kimi:latest`:

- `@moonshot-ai/kimi-code` pinned at **2.2.0** (`/opt/kimi-cli/package.json`
  version and dependency both 2.2.0).
- `config/config.toml` pins **`max_output_size = 131072`** on the
  FrontierCODEK model (line 60, rationale comment above it).
- The WI#411 scrub is present: both `entrypoint.sh` modes
  `unset KIMI_CODE_DANGEROUS_COMMAND_GUARD KIMI_CODE_TRUST_WORKSPACE`
  (lines 268-270, 289-291), and `kimi_task_executor.ts` deletes both vars
  after every env merge (lines 301-308).
- `lunarwing-worker-opencode2:latest` reports `opencode v2.0.24` (the
  in-image `opencode --version` contract check also ran during the build;
  item 71's 2.0.32 bump is still an open goal, so 2.0.24 is the expected
  pin).

## Runtime verification

`start-tenant fleetfoot` (`START_EXIT=0`) brought up, via the tenant's
systemd **user** manager with Quadlet containers: PostgreSQL, daemon,
xmpp-bridge, weechat + WS adapter, LunarVision sidecar (healthy,
20035/20036), and the three worker containers (`lunarwing-pebble-fleetfoot`
:10038, `lunarwing-opencode2-fleetfoot` :51030, `lunarwing-kimi-fleetfoot`
:30030) — every unit `active` in `mt-admin status fleetfoot`. SSH harness:
key uploaded to the secrets store, staged key deleted, agent bridge
`running` with `keys_loaded=1` (`/agent/status` on :10031 with the
`SSH_API_TOKEN` bearer). A stop/start cycle for the A1 fix came back
`RESTART_EXIT=0` with everything healthy.

- Gateway `:10030/api/health` → `{"status":"healthy"}` (no auth); API
  surface with the `GATEWAY_AUTH_TOKEN` bearer works
  (`/api/gateway/status`: version 2.1.6, llm_backend `openai_compatible`,
  model `tensorzero::function_name::lunarwing`, enabled channels
  unix_socket_repl / multica / weechat / xmpp / http / gateway, skills on).
- **Extensions (13 active)**: `study_notes` MCP (authenticated, read-only,
  5 tools exposed) + 9 WASM tools + 3 WASM channels — all `active` in
  `/api/extensions`.
- **Knowledge seed** (first boot, one-shot): **1623 notes seeded, 198
  skipped (already present), 20 failed** (import-level warnings —
  non-lowercase tags; plus one note with an embedded NUL byte, see finding
  F1), recorded in `state/knowledge_seed_state.json`; seed will not rerun.
  The **knowledge sweep** ran its first pass at 09:10:32Z
  (`state/knowledge_sweep_state.json`).
- **WI#357 exposure audit**: `mt-admin worker-listeners fleetfoot` →
  `worker listener audit: clean` — all three workers publish host-loopback
  only; no self-exposed inbound path.
- **WI#380 / item 44**: production `handle_message_timeout` default is
  630 s (`ic/src/settings.rs:723`, `default_handle_message_timeout()`);
  per-tenant settings store empty, code default applies.
- **Host-global health pipeline** (items 22/51 lane): the systemd
  `lunarwing-mt-health.timer` fires every 5 min and its 09:30 run
  (`status=0/SUCCESS`) already covers fleetfoot — probing every fleetfoot
  unit through the tenant's own user bus (`systemctl --user` via
  `sudo -u fleetfoot`, exactly the MT hard-rule shape). fleetfoot `runtime`
  is degraded only on the unconfigured multica channel (A4); `knowledge` is
  critical on the F1 sync failure below.

## Landed-changes exercise matrix

| Landed change (goal item) | Evidence on fleetfoot |
| --- | --- |
| WI#330 no-default LLM endpoint + worker inheritance (10) | `KIMI_BASE_URL`/`OPENCODE2_BASE_URL` written with `*_SOURCE=inherited`; after the A1 fix the inherited URL is proven container-reachable (probe from a rootless container: HTTP response in 14 ms) |
| kimi worker @ kimi-code 2.2.0 (4, WI#324) | image pin verified in-image; real job `3a170855` completed on the lane |
| kimi `max_output_size=131072` (53, WI#412) | `config/config.toml:60` in-image pin verified |
| WI#411 `KIMI_CODE_TRUST_WORKSPACE` scrub (52) | both entrypoint modes `unset` it; executor deletes it post-merge (in-image grep) |
| opencode2 lane (2: frozen v1, v2 selected) | `--with-opencode2` provisioned, fat image, job `d709abe4` completed on the lane; image carries `opencode v2.0.24` |
| pebble worker lane | container + dispatch + exec + result-forwarding verified (job `93a776bd`); fails only on the operator nanogpt key (A3) |
| WI#357 workers never self-expose (21) | `worker-listeners fleetfoot` → clean; opencode2 build-time `exposure_policy_assert` + `compile_out_remote_tunnel` self-tests green |
| Approval gate / pending state | `create_job` parked the turn at the `approval` gate (`/api/chat/pending-gates` showed the full parameter payload); `action=always` via `/api/chat/approval` resumed it and covers later calls in the thread |
| Knowledge seed + sweep + study MCP (5/6 lane) | 1623 seeded / sweep ran / `study_notes` extension active; **defect found in the additive refresh (F1)** |
| WASM toolchain lanes (channels/tools) | 3 channels + 9 tools installed and active |
| Health/self-heal pipeline (22, 51/WI#410) | systemd timer green, covers fleetfoot, probes via the tenant user bus |
| Engine v2 turn lifecycle | turns transition to `Completed` with responses in history; SSE gate events observed via pending-gates |
| **WI#371 cron control persistence (65)** | live route contract verified: `GET /api/workers/kimi/cron` → 404 "External worker 'kimi' is not configured for cron mode"; unknown worker → same 404 (the no-probing property). No cron-mode worker configured on this tenant, so the fail-closed/durability behaviors rest on item 65's in-tree integration pins (31/31) — the `durable` flag surfaces through this route and the owner-only `cron_mission` tool |
| **WI#364 orchestrator import pre-injection (63)** | in-tree at `ic/crates/lunarwing_engine/src/executor/scripting.rs` (`inject_missing_imports`, wired into the single CodeAct choke point); not gateway-chat-reachable (no code-exec tool on the gateway surface) — item 63's 71/71 targeted tests incl. the WI's exact `asyncio.gather` scenario stand as the evidence |
| **WI#408 channel WASM host credential redaction (50)** | in-tree at `ic/src/channels/wasm/wrapper.rs` (`redact_credentials`/`redact_credential_bytes`/`redact_headers_json` + `credential_echo_pins` tests); all 3 WASM channels + 9 tools on this tenant run through that wrapper |
| WI#390 mt-admin env-read hardening (43) | exercised for real: `configure-kimi`/`configure-opencode2` on fleetfoot went through the `_snapshot_tenant_env` path and wrote cleanly |
| WI#416 weechat-relay tenant-drop hardening (47) | exercised for real: add-tenant's weechat relay bootstrap completed with the tenant-side drops; weechat + adapter units active and the channel reports healthy |
| WI#415 OpenRC interpreter pinning (56) | N/A on this systemd host (OpenRC-only change; nothing to exercise) |
| WI#360 cargo-deny unsound/bans (60) | host/CI tooling, not tenant runtime; not exercised here |

## Job dispatch end-to-end (the core of the item)

Driven through the real path (gateway chat → LLM loop → `create_job` →
orchestrator on-demand WS dial → worker bridge → container executor →
job_events → assistant reply), thread
`bac0fdeb-37ac-4c97-9bdc-725b51693883`:

- **kimi** — job `3a170855-b85e-4a7b-8214-273d5192856e`: events show
  `[Bash] {"command":"echo KIMI_WORKER_OK"}` → `[tool result] KIMI_WORKER_OK`
  → `result: "Completed: Output: KIMI_WORKER_OK — the shell command ran
  successfully."` Assistant closed the loop in chat with the job id. **Real
  worker evidence on a fat image built from this tree (kimi-code 2.2.0 +
  WI#411/WI#412 pins in-image).**
- **opencode2** — job `d709abe4-04d2-4db7-9c13-b4e80020c0da`: streamed
  chunks show the worker executing and reporting; `result: "Completed: The
  command executed successfully. Exact stdout: OC2_WORKER_OK …"`.
- **pebble** — job `93a776bd-80e1-4725-9e43-845aece6b47d`: dispatch, exec
  spawn, and result forwarding all worked; the job failed with
  `required API key is not configured` (`result: "Failed: pebble exited
  with code 1"`) — the pebble worker's nanogpt credential is unset (no
  `pebble.env`; lane verified without it, same standing condition as
  item 72's tenants).

## Anomalies and follow-ups

- **F1 (NEW, tenant-visible defect — knowledge additive refresh poisoned by
  one bad corpus note):** the shared study-notes checkout's
  `agent-notes-by-agent/sweetiebot/lorebook-injection-defense-and-cache.md`
  contains a **raw NUL byte** at byte 2890 (inside a code span meaning to
  show a regex, `</?[\x00s]*lore_entry` — the author embedded a literal NUL
  instead of the escape text). PostgreSQL text columns reject NUL, so the
  note's write fails deterministically (`stranded-row write failed: Search
  failed: Update failed: db error`). The one-shot **seed** absorbs this as a
  per-note warning (1 of the 20 `notes_failed`), but the hourly **additive
  refresh** fails the *entire run* on it: `knowledge_sync_state.json` shows
  `consecutive_failures: 5`, `last_error_class: "note_import_failed"`,
  `notes_added_last_run: 0`, `last_success_at: null`, with exponential
  backoff retries (61→127→247→487→967 s) that can never succeed while the
  note is in the corpus. The host health pipeline reports fleetfoot
  `knowledge: critical` for exactly this. Two follow-ups: (a) corpus hygiene
  — strip/escape the NUL in the lw2studymcp note (also affects any other
  tenant seeding/syncing from the current checkout generation); (b)
  robustness — the refresh should treat a deterministic per-note import
  failure like the seed does (skip-and-count), not fail the whole run.
  Serving continues from the last good corpus in the meantime ("serving
  last good corpus"), so the tenant is functional; the seed's 1623 notes
  are all in place.
- **A1 (host provisioning gap, fixed for fleetfoot on the spot):** the
  item-72 runbook's `--llm-base-url http://192.168.1.157:3000/openai/v1` is
  the Gentoo host's LAN address and is **wholly unreachable from this host**
  (no LAN route; 100% packet loss). The tenant's first chat turn failed on
  transport after 3 provider retries. Correct upstream here is the
  WireGuard mesh address `http://100.88.232.146:3000/openai/v1` (what
  antelope/barracuda/spitfire use). Fixed for fleetfoot via
  `LLM_BASE_URL` (tenant-side env edit) + `configure-kimi` /
  `configure-opencode2` + restart; verified container-reachable and
  end-to-end by the three job dispatches. **Note: tenant `hermes` still has
  the stale LAN `LLM_BASE_URL`** — pre-existing, not changed by this item;
  its health lane already shows runtime degraded (pending-gates
  diagnostics unanswered, xmpp bridge not running). Recommend the operator
  repoint hermes the same way.
- **A2 (operator credential):** `fleetfoot@xmpp.sobe.world` XMPP auth fails
  (`NotAuthorized`, 30 s retry loop) — the account does not exist on the
  server. Same manual step as item 72's soarin; needs the XMPP server
  operator to create the account. The bridge unit itself is active and
  omemo-ready (100 prekeys) — only upstream auth is missing.
- **A3 (operator credential):** pebble needs
  `configure-pebble fleetfoot --nanogpt-api-key <key>` before its jobs can
  call an LLM (lane verified without it; `pebble.env` intentionally absent).
- **A4 (cosmetic, expected):** multica channel reports unhealthy (no multica
  server configured for this tenant) — the single "gateway channels
  unhealthy" issue in the fleetfoot runtime health line.
- **A5 (cosmetic):** the start-tenant SSH readiness line printed
  `keys_loaded=` (empty) at its one-shot check; a direct `/agent/status`
  probe right after showed `running` with `keys_loaded=1` — the check races
  the agent restart it triggers itself. Cosmetic timing note only.
- **Not reproduced here:** item 72's A7 (v1 `/api/chat/history` mislabeling
  in-flight engine-v2 turns as `Failed`). Turn 0 on this tenant shows
  `Failed` legitimately (the pre-A1-fix transport failure); turns 1-3
  completed and are labeled correctly. The mislabel path wasn't re-exercised
  (no turn was observed mid-flight through the shim this time).

## Disposition

The tenant is **left running** (all services boot-persistent under the
systemd user manager: `systemctl --user enable` + linger) for maintainer
inspection:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh status fleetfoot
uid="$(id -u fleetfoot)"
sudo -u fleetfoot XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user status lunarwing-fleetfoot.service
sudo -u fleetfoot XDG_RUNTIME_DIR="/run/user/$uid" podman ps
```

It is not an animal name and comes from the WI#273 list, so the teardown
routine must not target it. To decommission later anyway:
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant fleetfoot --purge`.
