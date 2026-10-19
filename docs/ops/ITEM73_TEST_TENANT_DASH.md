# Item 73 — Temporary Test Tenant `dash` (v2.1.6.0 landed-changes validation)

> Provisioned 2026-10-19 by Rarity (autonomous), branch
> `rarity/kimi-73-20261019-0700` @ `d450bc42b` (= `origin/master` + goals-doc
> commits). The tenant runs exactly this tree: mt-admin's `clone_tenant_repo`
> clones the running checkout's current branch.
>
> **This tenant is deliberately left running.** The name `dash` comes from the
> sanctioned test-tenant list in [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider) and is not
> an animal name, so the animal-name teardown routine must not target it.

## Purpose

Item 73 of `docs/ops/AGENT_GOALS_2.1.6.0.md`: stand up a temporary test tenant
with the **kimicode**, **opencode2**, and **pebble** external workers enabled
with full toolchains, exercise the changes that landed from the goal document,
and report. Sibling items 72 (soarin, Gentoo) and 74 (fleetfoot, Arch/systemd)
covered earlier windows; this tenant's headline delta is the **opencode2
v2.0.32** pin (WI#427, items 66/71) on a **freshly built Gentoo/OpenRC host** —
fleetfoot's image still carried v2.0.24 — plus the first item-7x OpenRC tenant
past WI#415 (item 56, OpenRC interpreter pinning).

## Host

Gentoo dev VM `colgatev` (OpenRC first-class, kernel 6.18.35-gentoo), rootless
podman, 17 GB RAM, 124 GB free disk, **fcron** as the host scheduler (no
cronie). Registry `/etc/lunarwing/ports.json` was empty before provisioning —
`dash` is the first tenant on this host, so this run also exercised the
first-tenant host bootstrap path (subuid/subgid, vision sidecar + sandbox
image builds, health-pipeline install) end to end.

## Provisioning record (add-tenant)

Command:

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  bash ic/scripts/lunarwing-mt-admin.sh add-tenant dash \
  --docker-group \
  --xmpp-jid dash@xmpp.sobe.world \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-pebble --with-kimi --with-opencode2 --with-study-mcp
```

- `--llm-base-url` points at the WireGuard-mesh TensorZero upstream
  (`100.88.232.146:3000`) — verified working keyless from this host before
  provisioning (a chat-completions round-trip returned a completion). The
  item-72 LAN address (`192.168.1.157:3000`) also answers here, but the mesh
  address is the one item 74 proved correct for workers, so it was chosen.
- Port block **10000-10009**: gateway 10000, http 10001, bridge 10002,
  postgres 10003, proxy 10004, weechat 10005, orchestrator 10006,
  pebble_wss 10008, weechat_adapter 10009 (opencode2_wss 51000, kimi_wss
  30000 from the dedicated ranges).
- Workers selected: `pebble`, `kimi`, `opencode2` (opencode v1 intentionally
  not selected — item 2 froze the v1 lane).
- `--with-study-mcp`: shared lw2studymcp checkout synced to
  `/var/cache/lunarwing/lw2studymcp`, tenant knowledge seed armed, and the
  additive Study Notes refresh scheduled **hourly at :17 via fcrontab**
  (mt-admin's scheduler autodetect preferred cronie/crond, found neither, and
  fell back to its managed-fcron path — the designed behavior on this host).
- **WI#330 verification** (first landed change re-verified live, before any
  start): `LLM_BASE_URL=http://100.88.232.146:3000/openai/v1` and
  `OPENCODE_BASE_URL` / `KIMI_BASE_URL` / `OPENCODE2_BASE_URL` all written as
  the tenant's own LLM endpoint with `*_SOURCE=inherited` markers.
- **WI#415 / item 56 exercised at render time** (OpenRC-only change, N/A on
  fleetfoot): the rendered `/etc/init.d/lunarwing-weechat-dash` bakes the
  canonical `/usr/bin/python3` interpreter at both weechat render sites
  (`command_args` line 22 and the fallback helper line 41) — no
  caller-resolved `$(command -v python3)` leakage into the unit.
- SSH harness provisioned (ed25519 keypair for `dash`, public key added to
  authorized_keys, private key staged for upload to the secrets store).
- `--docker-group` warned `group 'podman' does not exist; skipping` — this
  host has no podman socket group; per-tenant rootless stores don't need it
  (no impact observed; recorded as a cosmetic host note).
- Doctor gate before provisioning: 31 pass / 7 fail, where the 7 fails were
  exactly the fresh-host bootstrap set (5 missing images + unscheduled health
  pipeline) — all resolved by add-tenant/build-tenant below.

## Build record (build-tenant)

Command (tmux, flock-serialized):

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  bash ic/scripts/lunarwing-mt-admin.sh build-tenant dash \
  --with-wasm --require-wasm --with-pebble --with-kimi --with-opencode2 \
  --with-toolchains
```

- Cgroup build bounding logged `setpriv not found` → `running nice-only`.
  This host's util-linux lacks the Gentoo `caps` USE flag (documented
  prerequisite, `docs/ops/guide-for-spin-up-gentoo-tenants.md` §notes); the
  nice-only fallback is the designed degradation, recorded as a host finding
  rather than fixed (rebuilding util-linux mid-run risks destabilizing the
  host).

(build results: see the next revision of this file)

## Runtime verification

(to be filled from the live tenant)

## Landed-changes exercise matrix

(to be filled from the live tenant)

## Job dispatch end-to-end

(to be filled from the live tenant)

## Anomalies and follow-ups

(to be filled)

## Disposition

The tenant is **left running** (all services boot-persistent under OpenRC)
for maintainer inspection:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh status dash
```

It is not an animal name and comes from the WI#273 list, so the teardown
routine must not target it. To decommission later anyway:
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant dash --purge`.
