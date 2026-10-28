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

### F1 (confirmed still present on the 2.1.7 tree — worker source-root asymmetry)

The item-99 (thunderlane) report recorded this as run-208 **NT-8** and left it
as a follow-up candidate. It is **unchanged in the v2.1.7.0 tree**, and this
pass reproduces it with two different live revisions, which the prior pass
could not do:

- `build_pebble_worker` (`ic/scripts/mt-admin-lib/05-tenant-build.sh:1142`)
  builds from `${LUNARWING_ROOT}/pebble4lunarwing`, where
  `LUNARWING_ROOT="$(cd -- "${REPO_ROOT}/.." && pwd)"`
  (`ic/scripts/lunarwing-mt-admin.sh:16`) — i.e. the **operator's live
  checkout**, and stamps `SOURCE_REVISION` from
  `_worker_source_revision "$LUNARWING_ROOT" pebble4lunarwing`.
- `build_kimi_worker` is instead called with the **tenant clone**:
  `build_kimi_worker "false" "$with_toolchains" "" "$(tenant_lw_root "$name")"`
  (`05-tenant-build.sh:574`), as are `build_opencode_worker` (`:568`) and
  `build_opencode2_worker` (`:578`). pebble is the only worker lane that
  ignores the tenant root.

Measured during this build:

| source root | revision | branch |
| --- | --- | --- |
| operator checkout `/home/sweetiebot/lunarwing2` (pebble builds from here) | `2c86f2bc6` | `sweetiebot/kimi-99-20261028-0605` |
| tenant clone `/home/mistyfly/lunarwing` (kimi builds from here) | `62bed7142` | `sweetiebot/kimi-99-20261028-0605` |

The two differ because `add-tenant` cloned at `62bed7142` and this report's
skeleton commit `2c86f2bc6` landed afterwards. So the pebble image for this
tenant is built from a **different, later, and dirty** tree than the kimi image
and than the daemon binary that the tenant actually runs. Because
`_worker_source_revision` appends `-dirty` when `git status --porcelain` over
the subdir is non-empty, an operator's uncommitted `pebble4lunarwing/` edits
silently land in a tenant's worker image — the reverse of what
`upgrade-tenant`'s clean-tree gate is there to prevent. Consequence for
reproducibility: the pebble image cannot be traced to the tenant's own pinned
clone. Fix shape: give `build_pebble_worker` the same tenant-root parameter the
other three worker builders take (and pass `$(tenant_lw_root "$name")` at
`:562`), keeping the operator root only for the standalone
`build-pebble-worker` subcommand.

The same asymmetry exists on the **`upgrade-tenant`** path: at
`05-tenant-build.sh:1074` `build_pebble_worker "false" "$with_toolchains"` again
takes no source root, while `:1080`/`:1086`/`:1092` pass `$worker_source_root`.
So an in-place upgrade re-builds kimi/opencode/opencode2 from the tenant's
freshly checked-out `--target` ref but rebuilds pebble from whatever the
operator's live checkout holds at that moment — defeating the target-pinning the
upgrade path exists to give. Both call sites need the fix.

_(build artifacts recorded below once `BUILD_EXIT` is known)_

## F2 — the four prior-run findings are all fixed in this tree (regression-verified)

The v2.1.6.0 item-99 (`thunderlane`) pass filed four findings. Each has since
landed, and every fix commit is an ancestor of this branch
(`git merge-base --is-ancestor <sha> HEAD` → true for all of them):

| prior finding | fix | landed commits |
| --- | --- | --- |
| **F2 / WI#472** — approval without `thread_id` returned `202` but no-op'd, leaving the turn wedged at the gate | `resolve_approval_thread()` resolves the owning thread from `request_id` server-side (engine gate first, then the legacy awaiting-approval session), **404** when no live gate matches, **400** when the supplied `thread_id` does not own it | `6e950cb85`, `cbab67643`, `27502a00a`, `9fc305597`, `bd91d2e28` |
| **F5 / WI#471** — a quoted `NANOGPT_API_KEY` passed through `--env-file` verbatim, so pebble jobs failed with an opaque `401 Invalid session` | `configure_pebble` rejects literal quotes at either end, leading/trailing whitespace, embedded newlines, and a pasted `NANOGPT_API_KEY=` / `export NANOGPT_API_KEY=` assignment line | `adce2430b`, `73f308746` |
| **F4 / WI#473** — stale `shell` gates lingered until the 30-minute expiry instead of being reaped when their turn ended | gates are reaped when turns terminate, with a shared fallback classification and paused-turn preservation | `c3b209b15`, `e51099bbc`, `201348350`, `29a480c52`, `6b5d2892a` |
| **F1 / WI#436 item 1** — a raw NUL byte in one corpus note poisoned the whole additive knowledge refresh | **NOT fixed — reproduced again on this tenant** (see F3 below) | — |

Code anchors verified on this tree:
`ic/src/channels/web/server.rs:1565` (`resolve_approval_thread`, with the
comment at `:1500` explicitly citing the 202-no-op defect),
`ic/scripts/mt-admin-lib/09-worker-config.sh:1210-1229` (`configure_pebble`
validation block), `ic/src/bridge/router.rs:3682` (`drain_expired` on ingress).

### F2a — WI#471 guard regression-tested live on mistyfly

Rather than trust the merge-base check, all four rejection arms were exercised
against the freshly provisioned `mistyfly` (before any real key was configured),
via `sudo bash ic/scripts/lunarwing-mt-admin.sh configure-pebble mistyfly
--nanogpt-api-key <value> --model openai/gpt-5.2`:

| supplied value | exit | message |
| --- | --- | --- |
| `"sk-nano-fakequoted"` | 1 | `configure-pebble: pass the raw NANOGPT_API_KEY without literal quotes at either end` |
| `sk-nano-fake'` (trailing quote only) | 1 | same quote refusal |
| `NANOGPT_API_KEY=sk-nano-fake` | 1 | `configure-pebble: pass only the raw NANOGPT_API_KEY value, not an env-file assignment` |
| `<space>sk-nano-fake` | 1 | `configure-pebble: pass the raw NANOGPT_API_KEY without leading/trailing whitespace` |

After all four refusals `env/pebble.env` still did **not** exist — the guard
fails closed before writing, so the exact thunderlane F5 failure mode (a
quoted key silently persisted, worker recreated, then opaque 401s on every
job) is no longer reachable by accident. This is the prior pass's most
operator-visible defect and it is now closed at the point of entry.
