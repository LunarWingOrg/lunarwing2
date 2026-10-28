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

**The build log itself then proves the drift is not hypothetical.** The stamped
`SOURCE_REVISION` build args came out as:

```
=== Building pebble worker image ===
pebble worker image built: lunarwing-worker-pebble:latest
  [fat (with Rust/Go/C++ toolchains)]
  (revision 5cbcf817ac4dcc031d5fd74bf2c4e44d2e918d82)
=== Building kimi worker image ===
building kimi worker Docker image [fat ...] (kimi-code version: Dockerfile
  default, source revision: 62bed7142a20c40c268603a9b53fb3c80536c2c7) ...
```

`5cbcf817a` is **this run's own commit** ("item-99: verify prior-run findings
WI#471/#472/#473 fixed; WI#436 item1 still open"), made *while* the build was in
progress and touching only `docs/ops/ITEM99_TEST_TENANT_MISTYFLY.md`. So the
pebble image for tenant `mistyfly` is stamped to a moving operator HEAD that did
not exist when the tenant was provisioned, whereas the kimi image is stamped to
the tenant's pinned clone `62bed7142`. Two worker images serving the same tenant,
built from two different revisions, with neither matching the daemon binary the
tenant runs. That is a real traceability gap for a tenant upgrade or an incident
forensic, not just a docs delta.

The same asymmetry exists on the **`upgrade-tenant`** path: at
`05-tenant-build.sh:1074` `build_pebble_worker "false" "$with_toolchains"` again
takes no source root, while `:1080`/`:1086`/`:1092` pass `$worker_source_root`.
So an in-place upgrade re-builds kimi/opencode/opencode2 from the tenant's
freshly checked-out `--target` ref but rebuilds pebble from whatever the
operator's live checkout holds at that moment — defeating the target-pinning the
upgrade path exists to give. Both call sites need the fix.

### Build artifacts

`BUILD_EXIT=0`. Phases in order:

1. `building lunarwing for mistyfly` → `Finished release profile [optimized]
   target(s) in 9m 13s`.
2. `building xmpp-bridge for mistyfly`.
3. `building WASM extensions for mistyfly` → `All WASM extensions built
   successfully.`
4. `installing WASM extensions for mistyfly` →
   **`WASM install for mistyfly: 12 installed, 1 skipped (not built)`** — the
   same 12/1 split the prior item-99 pass recorded (the skip is the `darkirc`
   channel, correctly not built for a tenant provisioned without
   `--enable-darkirc`).
5. `=== Building pebble worker image ===` → fat, with the WI#334
   task-shell-toolchain smoke gate (`smoke-checking task-shell toolchain anchors
   (WI#334) ...`) passing before the `:candidate` tag is promoted to `:latest`.
6. `=== Building kimi worker image ===` → fat.


## F2 — BLOCKER: WI#424 egress fail-closed deadlocks worker start on rootless podman (never before exercised)

This is the headline finding of the pass. `start-tenant mistyfly` **aborted
before creating either worker container**, so a tenant provisioned exactly per
the item's recipe comes up with no kimi and no pebble worker on this host.

### What happened

`start-tenant` reached the pebble worker, distributed the image into the
tenant's rootless store, and then died:

```
distributing image lunarwing-worker-pebble:latest into mistyfly's rootless
  store (save|load — minutes for large images) ...
image lunarwing-worker-pebble:latest available in mistyfly's store
error: worker egress filter for 'mistyfly' did not survive the load with both
  exact reject rules (fail closed)
```

State after the abort: `lunarwing-pg-mistyfly` and `lunarwing-vision-mistyfly`
running; **no** `lunarwing-pebble-mistyfly` and **no**
`lunarwing-kimi-mistyfly` container; the daemon itself running and answering
`GET :10040/api/health` → `{"status":"healthy","channel":"gateway"}`; and the
worker OpenRC units never rendered at all (`/etc/init.d/` has no
`lunarwing-pebble-mistyfly`).

### Why it is new: this path had never been exercised on this host

`mistyfly` is the **only** tenant on this host with an egress subnet allocated:

```
antelope     subnet=unallocated   status: not configured
scootaloo    subnet=unallocated   status: not configured
barracuda    subnet=unallocated   status: not configured
thunderlane  subnet=unallocated   status: not configured
mistyfly     subnet=10.89.10.0/24 network: present
```

The prior item-99 tenant (`thunderlane`) and every sibling predate WI#424's
`f5a3988f6` ("item-46: enforce egress on every worker start path", 2026-10-26),
so their workers were started without any egress filter and the enforcement
path was never taken. The subnet is allocated lazily by `prepare_worker_egress`
(`15-worker-egress.sh:770`) on the **first worker start after the code landed**,
which is why only this fresh tenant hits it. Consequence: **every existing
worker tenant on this host is running unfiltered**, and the first restart of
any of them will now take this same path.

### Root cause

`worker_egress_apply` (`ic/scripts/mt-admin-lib/15-worker-egress.sh:560-572`)
performs the load and the verification as **two separate invocations** of
`worker_egress_nft`:

```bash
worker_egress_nft "$name" -f "$ruleset_path" || die "failed to load ..."
verified="$(worker_egress_verify "$name")"   || die "... did not survive the load ..."
```

and on a rootless-podman host `worker_egress_nft` (`:396-408`) wraps **each**
call in its own `sudo -u <t> … podman unshare --rootless-netns nft …`. The
rootless netns only persists while podman's keeper process is alive. When the
keeper is not running, each invocation gets a **fresh** netns, so the rules load
into one namespace and the verification reads a different, empty one.

Measured directly — two consecutive invocations landed in different namespaces:

```
invocation 1: net:[4026536664]
invocation 2: net:[4026536725]
```

and load-then-verify as two invocations failed even with the netns explicitly
pinned via `ROOTLESS_NETNS`:

```
--- load ---        LOAD_OK
--- verify (separate invocation) ---
Error: No such file or directory
list chain inet lunarwing_worker_egress fwd_mistyfly
```

Doing load **and** verify inside a **single** `podman unshare --rootless-netns`
session succeeds immediately, proving the ruleset itself is correct:

```
loaded; now verify in SAME session:
table inet lunarwing_worker_egress {
    chain fwd_mistyfly {
        type filter hook forward priority filter; policy accept;
        iifname "lweg10" meta nfproto ipv6 counter packets 0 bytes 0 reject with icmpx admin-prohibited
        iifname "lweg10" ip saddr != 10.89.10.0/24 counter ... reject with icmpx admin-prohibited
        iifname "lweg10" ip saddr 10.89.10.0/24 ct state established,related accept
        iifname "lweg10" ip saddr 10.89.10.0/24 ip daddr 100.88.232.146 tcp dport 3000 accept
        iifname "lweg10" ip saddr 10.89.10.0/24 counter ... reject with icmpx admin-prohibited
```

Once a keeper exists, the netns is stable and `worker-egress-apply` succeeds
repeatably (3/3). podman logs the recovery explicitly:

```
level=warning msg="rootless netns program no longer running, trying to start it again"
```

So the defect is a **keeper-warm-up race between two invocations**, not a bad
ruleset, not a missing `nft`, and not a network-contract mismatch. The rendered
ruleset is correct and contains exactly the allowlist entry the tenant needs
(the WireGuard TensorZero endpoint `100.88.232.146:3000`) plus the required
exact reject arms.

Note the contrast with the guard that OpenRC/Quadlet actually run:
`lunarwing-worker-egress-guard.sh` does its load and both `verify_chain` calls
in **one** process, with `nft_in_netns` resolving the namespace once per call
inside that single invocation — which is why the *boot* path is designed to
work while `worker_egress_apply` is not. The mt-admin function should either
delegate to the same guard (it already installs it on the line above) or run
load+verify in one netns session.

### F2b — the fail-closed `die` also orphans the tenant lifecycle lock

The `die` inside `worker_egress_apply` unwinds through `prepare_worker_egress`
→ the pebble worker create path → `_start_tenant_init_services`, which is
wrapped in `with_tenant_lifecycle_lock` (`04-tenant-lifecycle.sh:1018`). That
wrapper opens the lock fd with `exec {lock_fd}>…` and relies on `flock -u` plus
`exec {lock_fd}>&-` **after** the command returns
(`00-common.sh:134-147`). A `die` is a hard `exit`, so neither runs, and the
fd is inherited by every long-lived child started before it — the
`supervise-daemon` babysitters for pg, vision, weechat-adapter, xmpp-bridge and
the daemon itself.

Verified: those daemons hold fd 10 pointing at the lock, and the fd flags are
`0100001` — **`O_CLOEXEC` (`02000000`) is absent**:

```
sudo cat /proc/1270099/fdinfo/10
pos:    0
flags:  0100001
mnt_id: 30
ino:    134001
lock:   1: FLOCK  ADVISORY  WRITE 1268718 00:1c:134001 0 EOF
```

`flock(2)` locks belong to the open file description, which survives as long as
*any* process holds an fd to it. So the lock stays held by a pid that no longer
exists:

```
$ sudo awk -v h=134001 'index($0,h){print}' /proc/locks
36: FLOCK  ADVISORY  WRITE 1268718 00:1c:134001 0 EOF      <- holder: GONE
36: -> FLOCK  ADVISORY  WRITE 1339106 00:1c:134001 0 EOF   <- blocked: my retry
$ sudo flock -n /run/lock/lunarwing-tenants/mistyfly.lock -c 'echo GOT_LOCK'
BLOCKED (lock orphaned)
```

Every subsequent `start-tenant`, `stop-tenant`, `render-units`, `upgrade-tenant`
and `migration-export-boundary` for `mistyfly` blocks forever on
`flock -x`, because they all go through `with_tenant_lifecycle_lock`. The same
leak exists on `thunderlane` — six of its daemons hold fd 10 on
`thunderlane.lock` — it is simply not yet fatal there because that lock's
holder exited cleanly enough that `/proc/locks` shows it free.

Two-part fix shape:
1. Open the lifecycle lock fd `O_CLOEXEC` (bash `exec {fd}>file` cannot set it
   directly; use `flock`'s own fd handling, or `exec {lock_fd}<>"$lockfile"` in
   a subshell that never execs a daemon), so it cannot be inherited by
   `supervise-daemon`.
2. Make `with_tenant_lifecycle_lock` release on the `die` path too (`trap` on
   `ERR`/`EXIT` inside the wrapper), so a fail-closed abort does not wedge the
   tenant.

### Recovery used for this pass

Because `stop-tenant`/`start-tenant` are themselves blocked by the orphaned
lock, recovery had to bypass mt-admin and stop the leaking daemons through
OpenRC directly (`rc-service`), which is what mt-admin's `_openrc_service`
wraps anyway — a deliberate, recorded exception to the "always use mt-admin"
rule, forced by this bug. Afterwards the keeper is warm, the lock is free, and
`start-tenant` completes.

## F3 — the prior-run findings: three fixed, one still open

The v2.1.6.0 item-99 (`thunderlane`) pass filed six findings (F1-F6). Three of
the four with a named work item have since landed, and every fix commit is an
ancestor of this branch (`git merge-base --is-ancestor <sha> HEAD` → true for
all of them):

| prior finding | status | landed commits |
| --- | --- | --- |
| **F2 / WI#472** — approval without `thread_id` returned `202` but no-op'd, leaving the turn wedged at the gate | **fixed** — `resolve_approval_thread()` resolves the owning thread from `request_id` server-side (engine gate first, then the legacy awaiting-approval session), **404** when no live gate matches, **400** when the supplied `thread_id` does not own it | `6e950cb85`, `cbab67643`, `27502a00a`, `9fc305597`, `bd91d2e28` |
| **F5 / WI#471** — a quoted `NANOGPT_API_KEY` passed through `--env-file` verbatim, so pebble jobs failed with an opaque `401 Invalid session` | **fixed** — `configure_pebble` rejects literal quotes at either end, leading/trailing whitespace, embedded newlines, and a pasted `NANOGPT_API_KEY=` / `export NANOGPT_API_KEY=` assignment line | `adce2430b`, `73f308746` |
| **F4 / WI#473** — stale `shell` gates lingered until the 30-minute expiry instead of being reaped when their turn ended | **fixed** — gates are reaped when turns terminate, with a shared fallback classification and paused-turn preservation | `c3b209b15`, `e51099bbc`, `201348350`, `29a480c52`, `6b5d2892a` |
| **F1 / WI#436 item 1** — a raw NUL byte in one corpus note poisoned the whole additive knowledge refresh | **still open — reproduced on this tenant** (see F4 below) | — |

Code anchors verified on this tree:
`ic/src/channels/web/server.rs:1565` (`resolve_approval_thread`, with the
comment at `:1500` explicitly citing the 202-no-op defect),
`ic/scripts/mt-admin-lib/09-worker-config.sh:1210-1229` (`configure_pebble`
validation block), `ic/src/bridge/router.rs:3682` (`drain_expired` on ingress).

### F3a — WI#471 guard regression-tested live on mistyfly

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

## F4 — WI#436 item 1 still open: one bad corpus note still poisons the whole refresh

The corpus defect the prior pass recorded as F1 is **unchanged**:
`agent-notes-by-agent/sweetiebot/lorebook-injection-defense-and-cache.md` in the
shared checkout still carries a **raw NUL byte at offset 2890**, inside a code
span that intends to show the escape text `</?[\x00s]*lore_entry`:

```
bytes 4683 NUL at 2890
context b'aks any case-insensitive `</?[\\x00s]*lore_entry` opener with `&'
```

The published generation is `5857c69`, the same one `thunderlane` synced, so
nothing upstream changed. PostgreSQL `text` rejects NUL, so that note's write
fails deterministically.

The remedy WI#436 item 1 asks for — make the additive refresh treat a
deterministic per-note import failure the way the one-shot seed does
(skip-and-count) — has **not** landed. The refresh still fails the entire run on
any single note failure (`ic/src/workspace/knowledge_sync.rs:259-261`):

```rust
if import.notes_failed > 0 {
    return record_failure(config, state, "note_import_failed", Some(import));
}
```

Note the asymmetry the prior pass identified still holds: `knowledge_seed.rs`
absorbs per-note failures into a warning count, so the tenant boots with a
usable corpus, while `knowledge_sync.rs` escalates the same per-note failure to
a whole-run failure with exponential backoff that can never succeed while the
byte remains. `record_failure` grows `consecutive_failures` and
`retry_after_secs` indefinitely, so the tenant silently serves a
forever-stale corpus.

This is not new work for this pass — it is the same open WI#436 item, now
confirmed still open on the v2.1.7 tree.
