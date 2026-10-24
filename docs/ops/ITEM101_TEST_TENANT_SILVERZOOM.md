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

The first attempt's build was cut off mid-compile (`/tmp/build-tenant-101.log`
ends inside the dependency graph), so this pass re-ran the same command
incrementally: `/tmp/build-tenant-101b.log`, `BUILD_EXIT=0` at 15:42Z. The
daemon release binary and xmpp-bridge were already complete from the first
attempt (release build reused; `lunarwing --version` = 2.1.6); WASM extensions
and both worker images were built in this pass.

- Daemon release binary `/home/silverzoom/lunarwing/ic/target/release/lunarwing`
  (110 MB) + xmpp-bridge, tenant clone at `722d07a47`.
- **WASM lane: 12 installed, 1 skipped** (channels `multica`, `weechat`, `xmpp`;
  9 tools; darkirc correctly skipped — tenant darkirc disabled).
- Worker images, both **fat (Rust/Go/C++ toolchains)**, distributed into
  silverzoom's rootless store by `start-tenant`:
  - `lunarwing-worker-pebble:latest` (2.33 GB)
  - `lunarwing-worker-kimi:latest` (2.68 GB, kimi-code 2.3.0, revision
    `722d07a475fccf2588a5895233f5ce2ad9c50319`); the in-build `npm test`
    self-test suite ran during the image build.

### Toolchain probe (the item's "full toolchains" requirement)

| tool | `lunarwing-worker-kimi` | `lunarwing-worker-pebble` |
| --- | --- | --- |
| rustc / cargo | 1.96.1 | 1.96.1 |
| go | 1.24.3 | 1.24.3 |
| g++ | 12.2.0 (Debian 12.2.0-14+deb12u1) | 12.2.0 |
| python3 | present | present |
| node | 22.x | 18.x |

Toolchains live under `/usr/local/{cargo,go}/bin` and are on `PATH` for the
task runner's login shell; a non-login `sh -c 'which rustc'` looks like a
missing toolchain (same image shape as item 99).

## Runtime verification

`start-tenant silverzoom` → `START_EXIT=0` (`/tmp/start-tenant-101.log`).
PostgreSQL (10023), daemon, xmpp-bridge, weechat + WS adapter, LunarVision
(healthy, 20025/20026) and both workers (`lunarwing-pebble-silverzoom` :10028,
`lunarwing-kimi-silverzoom` :30020) are **active** under the tenant's systemd
**user** manager with Quadlet containers.

- Gateway `:10020/api/health` → `{"status":"healthy","channel":"gateway"}`.
- `/api/gateway/status` (bearer): version 2.1.6, `llm_backend`
  `openai_compatible`, model `tensorzero::function_name::lunarwing`, skills on.
- **12 WASM extensions active** (9 tools + 3 channels); no `study_notes` MCP on
  this tenant (no `--with-study-mcp`).
- **WI#357 exposure audit**: `worker-listeners silverzoom` → `worker listener
  audit: clean` — both workers publish host loopback only (10028 / 30020 plus
  in-container health 8443).
- `multica` reports unhealthy (no multica server configured — same cosmetic
  condition as every prior test tenant); weechat/xmpp/http/gateway healthy.
- **Agent LLM loop**: simple gateway turns answer `BOOT_OK` (state
  `Completed`), proving the tenant's own LLM path. Direct upstream probe with
  the tenant's `LLM_API_KEY` returned HTTP 200 for both
  `tensorzero::function_name::lunarwing` and
  `tensorzero::function_name::FrontierCODEK`.
- Pebble credentials: `configure-pebble silverzoom --nanogpt-api-key …` (key
  taken from the working sibling tenant `antelope`, **unquoted** — item 99 F5
  showed a quoted value poisons the lane with a generic 401), then
  `restart-worker silverzoom pebble`. `pebble.env` mode 0600.

## Worker task exercise

Driven through the real path: gateway chat (`POST /api/chat/send`) → agent LLM
loop → `create_job` → **approval gate** → `/api/chat/approval`
(`action=always`, with the owning `thread_id`) → orchestrator on-demand WS dial
→ worker bridge → container executor → `job_events` → assistant reply.

### kimi — smoke test (markers + toolchain versions)

Job `da98363c-5fc5-4e4c-bc72-3c185d265e58`, thread
`1d04a539-e92e-4911-94a1-3803effd823e`, `state: completed` (~66 s from job
creation to terminal). Worker events show the real in-container tool loop:

- `[Bash] {"command":"echo ITEM101_KIMI_SMOKE_OK"}` → `[tool result]
  ITEM101_KIMI_SMOKE_OK`
- `[Bash] rustc --version … go version … g++ --version` →
  `rustc 1.96.1 (31fca3adb 2026-06-26)` / `go version go1.24.3 linux/amd64` /
  `g++ (Debian 12.2.0-14+deb12u1) 12.2.0`, each re-run separately with
  `exit=0` after the worker decided it wanted per-command evidence.
- Result message repeats the marker and all three version lines, and states no
  files were written and no network touched (as requested).

### Findings so far

- **F1 (API ergonomics — `/api/chat/history` labels in-flight turns
  `Failed`):** `ic/src/channels/web/util.rs:186` sets `turn.state = "Failed"`
  for any user message without a response yet, so an in-flight or
  approval-parked turn reads as failed. The thread-level state
  (`/api/chat/threads`) is the reliable signal: the smoke turn sat at the
  `create_job` gate with `state: Failed` in history, and after approval the job
  ran and completed. Callers must not treat history `Failed` as terminal
  without checking thread state.
- **F2 (carried from item 99 — approval needs `thread_id`):** approvals here
  were issued with the owning `thread_id` and resumed the turn correctly;
  item 99's F2 (a `thread_id`-less approval returns 202 and no-ops) is still
  the documented behaviour to avoid.

_(further tasks appended below)_
