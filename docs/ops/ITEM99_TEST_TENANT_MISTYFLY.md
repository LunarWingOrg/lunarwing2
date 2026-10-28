# Item 99 (v2.1.7.0) — Test Tenant `mistyfly` (kimi + pebble external workers, full toolchains)

> Provisioned 2026-10-28 by SweetieBot (autonomous), branch
> `sweetiebot/kimi-99-20261028-0605`, at tree `62bed7142` (LunarWing 2.1.7).
> The tenant clone tracks that **`0605`** branch: mt-admin's `clone_tenant_repo`
> clones the running checkout's current branch.
>
> **Re-MR provenance (2026-10-28, branch `sweetiebot/kimi-99-20261028-1523`).**
> The `0605` MR was closed by the operator for merge conflicts, not for the
> findings. This document and the `worker_egress_launch_args` fix it surfaced
> were re-landed **unchanged** onto the current target `66e9433b1` — the test
> was *not* repeated and the tenant was *not* reprovisioned. Consequently every
> SHA and build-log excerpt below refers to the **original `0605` run's** trees
> (`62bed7142`, `2c86f2bc6`, `5cbcf817a`): they are the measured evidence for
> findings F1/F2/F2b/F4/F5 and are deliberately *not* rewritten to this
> branch's SHAs, since rewriting them would invalidate the traceability gap F1
> describes. Where this document says "this run" it means the `0605` run; the
> only content added on `1523` is this provenance note and the goals-doc DONE
> trail.
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

### F1 (confirmed still present on the 2.1.7 tree — worker source-root asymmetry) — **WI#507**

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


## F2 — BLOCKER: WI#424 egress fail-closed deadlocks worker start on rootless podman (never before exercised) — **WI#504**

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

**Correction (item 114, run-256 QV-1/QV-2):** the original contrast with
OpenRC/Quadlet was wrong: the guard's one shell process still called
`podman unshare --rootless-netns` three times (load, forward read, input read).
The WI#504 description repeated that mistake. Both paths needed a single
namespace session, and that alone was insufficient: without a running container,
Podman destroys the loaded namespace on session exit, before the first worker
starts. The rejected !2027 fixed the session split but not that lifetime gap.

**2026-10-28 follow-up (SweetieBot, item 114):** mt-admin now delegates to the
shared guard, whose load path first starts `lw-egress-keeper-<tenant>`, a pod on
the tenant egress network using Podman's locally built pause infra image (no
registry pull). It then loads and verifies both chains in one namespace session.
The keeper survives into worker start; post-start verification remains read-only
and fails closed. Tenant stop removes the keeper after writer quiescence;
teardown removes it before deleting the network. The current harness passes
147 hermetic checks plus 8 live checks on rootless Podman 5.8.2: an isolated
`liveprobe` fixture with no running containers or pre-existing keeper completes
real mt-admin apply → pre-guard → first container start → post-guard and
`worker_egress_audit`, then a cold restart after stopping both worker and keeper.
The namespace remains `net:[4026537014]` across the first start; allowlisted HTTP
works, raw-IP/DNS probes fail, and teardown removes the fixture keeper/network.
This is a BusyBox worker fixture under the agent's own rootless account, not a
new mistyfly run or a fully provisioned daemon/LLM tenant. Its account and state
paths are fixture bindings; the nft, Podman, apply, guard, post-start checks and
audit are real. The OpenRC/Quadlet guard contracts are covered by the hermetic
suite. The new sequence pin fails against the rejected guard and when only the
keeper ensure call is removed.

### F2a — the launch-args status bug this pass actually fixes in mt-admin (QQ-1)

F2 is the netns keeper race and is **not** the defect this MR patches; F5 calls
the launch-args bug "F2's bug", which conflates them. They are distinct, so it
gets its own finding here (run-250 review, QQ-1: the report never explained the
bug its own commit fixes, and without it a reader cannot tell why an otherwise
docs-only MR touches mt-admin at all).

**Symptom.** `start-tenant` aborts *immediately after a successful egress apply*
for any tenant whose allowlist resolves to **literal IPs only** — which is the
common shape whenever the tenant's LLM endpoint is an address rather than a
hostname, as it is on this host's WireGuard TensorZero endpoint
`100.88.232.146:3000`. `prepare_worker_egress` dies with
`cannot render worker egress launch arguments`, so the worker start is refused
even though the egress ruleset is live and correct.

**Mechanism.** In `worker_egress_launch_args`
(`ic/scripts/mt-admin-lib/15-worker-egress.sh`), the host-mapping emission is a
`while IFS= read -r mapping` loop over a here-string whose body is
`[[ -n "$mapping" ]] && printf …`. With an IP-only allowlist
`worker_egress_host_mappings` prints nothing, but the here-string still yields
one empty line, so the body's `[[ -n "$mapping" ]]` test is **false** on that
only iteration. Bash propagates the last command's status out of the `while`,
and that loop is the function's final statement — so the function returns **1**
*after* having already printed valid `--network`/`--dns` arguments. The caller
sees a nonzero status and treats the (correct) output as unusable.

Two sibling functions do **not** share the fault, which is why this is specific
to the launch-args path: `worker_egress_host_mappings` on an empty hosts file
and `_worker_egress_static_hosts` on IP-only records both return 0. And
`_worker_egress_llm_targets` already ends in an explicit `return 0` — the
convention this fix adopts.

**Fix (landed on this branch, commit `64442bffa`).** Append the trailing
`return 0`, with a comment naming the empty-mapping case and citing
`_worker_egress_llm_targets` as the precedent. The printed arguments were never
wrong; only the reported status was. Pinned by a four-assertion regression in
`ic/scripts/tests/test-worker-egress.sh` that drives the real function through
an emptied hosts file and asserts success, the DNS pin, and the absence of
`--add-host`.

**Relationship to F2 and F5.** This bug is what made F5's symptom *silent* on
mistyfly: the tenant had no NanoGPT host mapping, so it hit the IP-only shape,
so the launch args were rejected. F2 (netns race) blocked the apply itself and
is tracked separately at WI#504; F2a is fixed here and is the reason a docs-and-
script MR is the right vehicle for the finding rather than a follow-up.

### F2b — the fail-closed `die` also orphans the tenant lifecycle lock — **WI#505**

The `die` inside `worker_egress_apply` unwinds through `prepare_worker_egress`
→ the pebble worker create path → `_start_tenant_init_services`, which is
wrapped in `with_tenant_lifecycle_lock` (`04-tenant-lifecycle.sh:1018`). That
wrapper opens the lock fd with `exec {lock_fd}>…` and relies on `flock -u` plus
`exec {lock_fd}>&-` **after** the command returns
(`00-common.sh:134-147`). A `die` is a hard `exit`, so neither runs, and the
fd is inherited by every long-lived child started before it — the
`supervise-daemon` babysitters for pg, vision, weechat-adapter, xmpp-bridge and
the daemon itself.

This is **OpenRC-specific**: on OpenRC the daemons are started inside the locked
section as forked children of mt-admin, so they inherit the descriptor, whereas
on systemd hosts they are started by the user manager rather than as children of
the locked process and so do not. The `die` path wedges a tenant on OpenRC/Gentoo
but not on systemd — which is exactly why the fix belongs inside
`lunarwing-mt-admin.sh` behind its init abstraction rather than as a
host-specific workaround.

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

Fix shape — **one step, not two** (run-250 QQ-3). The original note here
proposed both an `O_CLOEXEC` open *and* a `trap` release on the `die` path; the
second half is unnecessary. A flock belongs to the open file description, so
once the descriptor cannot be inherited, a `die` is a hard `exit` that closes it
and the kernel releases the lock at process exit. No trap needed. Two
equivalent shapes:

1. **Close the descriptor in the subshell that starts each service** —
   `exec {LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD}>&-` before the `rc-service` or
   `systemctl` call — so `supervise-daemon` never sees it. (bash
   `exec {fd}>file` cannot set `O_CLOEXEC` itself, hence closing explicitly.)
2. **Hold the lock with `flock -o`** so the body runs with the descriptor
   already closed. This one has a caveat: the nested-lock validation
   `inherited_lock_fd_is_held` (`ic/scripts/mt-admin-lib/00-common.sh:84`,
   called from `:117`, `:125`, `06-env-generation.sh:1949` and
   `04-tenant-lifecycle.sh:1268`) must be kept working with the descriptor
   closed, or re-entrant lifecycle calls lose their re-entrancy check.

Either shape also removes the thunderlane-style latent leak noted above (six
daemons holding fd 10 on `thunderlane.lock`). Note this reworks the same
wrapper `with_tenant_lifecycle_lock` that the closed-unmerged !2010 (item 92)
also touched, so whoever lands it should reconcile against that branch.

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

Live state on `mistyfly`, showing exactly the predicted seed-vs-refresh
asymmetry:

`state/knowledge_seed_state.json` — the one-shot seed **succeeded**, absorbing
per-note failures as a count:

```json
{
  "seeded_at": "2026-10-28T06:42:36.063977186Z",
  "source_home": "/var/cache/lunarwing/lw2studymcp",
  "notes_seeded": 1795,
  "notes_skipped_existing": 26,
  "notes_failed": 20
}
```

`state/knowledge_sync_state.json` — the hourly additive refresh has **never**
succeeded and is in permanent backoff:

```json
{
  "schema_version": 1,
  "completed_source_revision": null,
  "last_attempt_at": "2026-10-28T07:19:09.313471829Z",
  "last_success_at": null,
  "consecutive_failures": 6,
  "last_error_class": "note_import_failed",
  "notes_added_last_run": 0,
  "notes_deferred_last_run": 0
}
```

`completed_source_revision: null` with six consecutive failures inside ~37
minutes of boot is the signature: the tenant is functional on its seeded corpus
but can never pick up a corpus update.

## F5 — WI#424 egress default allowlist has no pebble LLM host; every pebble job 401s/000s — **WI#506**

The pebble worker's model backend is the NanoGPT API at `nano-gpt.com` (the
`pebble` binary's baked-in default — no `PEBBLE_BASE_URL` is written by
mt-admin, and `_worker_egress_llm_targets`
(`ic/scripts/mt-admin-lib/15-worker-egress.sh:241`) reads exactly
`LLM_BASE_URL OPENCODE_BASE_URL OPENCODE2_BASE_URL KIMI_BASE_URL
PEBBLE_BASE_URL`, so with none of those naming a NanoGPT endpoint the rendered
egress ruleset only ever allowed the tenant's own TensorZero address
`100.88.232.146:3000`).

Result: on the first pebble dispatch after `start-tenant`, the container's
stderr showed

```
[stderr] [llm-request] request 1 started
[stderr] [llm-retry] attempt 1 failed; retrying
[stderr] [llm-retry] attempt 2 failed; retrying
[stderr] [llm-request] request 1 finished
[stderr] api failed after 3 attempts: http error: error sending request for url (https://nano-gpt.com/api/v1/messages)
Failed: pebble exited with code 1
```

and from inside the container `getent hosts nano-gpt.com` returned nothing
while `curl https://nano-gpt.com/api/v1/models` printed
`Could not resolve host: nano-gpt.com` (`HTTP=000`). The container had no
NanoGPT DNS entry because the egress host mapping was empty (see F2a's
`worker_egress_launch_args` status bug) and the network's recursive DNS is disabled by
design. Every pebble job failed instantly with `pebble exited with code 1`
(`external:pebble`, `elapsed 0`) — which reads as a worker crash but is really
the fail-closed egress firewalling the worker's *only* model backend.

Two-part fix shape:
1. **Operational (applied here):** `configure-worker-egress mistyfly --allow
   nano-gpt.com:443` re-rendered the ruleset with the NanoGPT static host
   mapping (`nano-gpt.com:216.150.1.1`), and `restart-tenant` force-recreated
   both worker containers to pick up the new `--add-host`. After that
   `curl https://nano-gpt.com/api/v1/models` in the container returned
   `HTTP=200` and the next pebble job completed.
2. **Code:** the pebble lane must not let a missing allowlist entry fail
   silently. **Prefer the loud shape (run-250 QQ-2, security):** at
   `configure-pebble` and at the pebble lane's start, check that the pebble
   backend host is among the tenant's egress targets (or that `PEBBLE_BASE_URL`
   is set, since `_worker_egress_llm_targets` already reads it) and **refuse**,
   with a message naming `configure-worker-egress --allow`. The allow then shows
   in the tenant's own allowlist where an operator can audit it.

   The tempting alternative — auto-including the NanoGPT host whenever
   `--with-pebble` is selected — is **not** recommended: it widens a fail-closed
   allowlist implicitly, and a later backend change baked into the `pebble`
   binary would widen it again without anyone choosing to. WI#424's whole value
   is that the allowlist is deliberate; a hidden default erodes exactly that
   property. This is a real provisioning gap either way: a fresh
   `--with-pebble` tenant on a host that already enforces WI#424 egress cannot
   run any pebble job until an operator allows NanoGPT — the question is only
   whether it fails loudly at provision time or silently per job.

This also explains why the prior-pebble tenants (thunderlane, silverzoom,
scootaloo, barracuda) never saw it: none of them has an egress subnet, so their
pebble containers still had default `pasta` DNS and reached NanoGPT directly.

## Verification (runtime)

`start-tenant mistyfly` → `START_EXIT=0` (after the F2 fix was in place), then
`restart-tenant mistyfly` → `RESTART_EXIT=0` (after the F5 allowlist change).
All services boot-persistent under OpenRC with `-sup` crash supervisors.

- Windows/PG: `lunarwing-pg-mistyfly` (10043), daemon `lunarwing-mistyfly`,
  `xmpp-bridge-mistyfly`, `lunarwing-weechat-mistyfly` +
  `lunarwing-weechat-adapter-mistyfly`, LunarVision sidecar (20045/20046).
- Gateway `:10040/api/health` → `{"status":"healthy","channel":"gateway"}`.
- `/api/gateway/status` (bearer) → version `2.1.7`, `llm_backend`
  `openai_compatible`, model `tensorzero::function_name::lunarwing`, channels
  `unix_socket_repl`, `multica`, `weechat`, `xmpp`, `http`, `gateway`.
  `multica` reports unhealthy (no multica server configured — cosmetic, as
  every prior test tenant).
- **13 extensions active**: `study_notes` MCP + 9 WASM tools
  (`cavepony`, `gitlab`, `gotify`, `llm-context`, `multica-bridge`, `ssh`,
  `vision-analyze`, `voice-transcribe`, `web-search`) + 3 WASM channels
  (`multica`, `weechat`, `xmpp`).
- **Workers** (both `external:`, health endpoints answer):
  - pebble `:20044/health` → `{"mode":"websocket","status":"ok",…,
    "version":"pebble-worker-0.1.0","worker_id":"worker-pebble-mistyfly"}`
  - kimi `:30041/health` → `{"status":"ok",…,"version":"kimi-worker-1.0.0",
    "kimi_code_version":"2.3.0","worker_id":"worker-kimi-mistyfly"}`
- **WI#357 exposure audit**: `worker-listeners mistyfly` → `clean`. Both
  workers publish host loopback only (10048/30040 + in-container 8443).
- **WI#424 egress**: `worker-egress mistyfly` → both containers
  `attached (exclusive, DNS pinned, static hosts current)`.

## WI#472 / WI#471 / WI#473 — live regression check on mistyfly

The three prior-run findings that had landed fixes were each exercised against
the live gateway:

- **WI#472 (approval thread resolution):** a bogus `request_id` approval now
  returns `404 No pending approval matches this request_id` (was a silent
  `202`); the valid id paired with the wrong `thread_id` returns
  `400 thread_id does not own this approval request` (was a silent `202`).
  Confirmed on both branches of `resolve_approval_thread`.
- **WI#471 (pebble key quoting):** all four `configure-pebble` rejection arms
  fire live (quotes each end, trailing quote only, `NANOGPT_API_KEY=` paste,
  leading whitespace), each `exit 1` and leaving no `pebble.env` behind — see
  F3a.
- **WI#473 (gate reap on turn termination):** not re-exercised in isolation
  because the dispatches used `action=always` with a pinned `thread_id` and no
  long-lived `shell` gates were left; the code (`c3b209b15`) is in-tree and
  the F4 gate was cleared on turn completion.

## Worker task exercise

Four distinct tasks (two per worker), deliberately harder and different from
the prior passes. Every task was dispatched through the real path: gateway
`POST /api/chat/send` → agent LLM loop → `create_job` (`mode: "<worker>"`,
`wait: true`) → approval gate → `POST /api/chat/approval` (`action=always`
with the owning `thread_id`) → resume → orchestrator WS dial → worker bridge →
container executor → job events → assistant reply.

### kimi — task A: thread-safe LRU cache with TTL (Rust, multi-module)

Job `b7998c0c…` (`external:kimi`, `completed`, 187 s). Worker tool loop:

- `[Bash] rustc --version; cargo --version …` → `rustc 1.96.1` / `cargo 1.96.1`
- `cargo new mf_kimi_lru --bin`, then `[Write]` of `src/ttl.rs`, `src/cache.rs`,
  `src/main.rs` across three modules, an `[Edit]` fixing `edition` 2024→2021,
  `cargo build --release` and `cargo test --release`.
- 8 unit tests **all pass**: `test result: ok. 8 passed; 0 failed`.
- CLI exercise over the required stdin produced one result line per command,
  all correct (`get a`→1, `get b`→2, first `len`→3, `evict`, second `len`→0,
  `get c`→3).
- The full `src/cache.rs` was returned and matches the spec (injected `Clock`
  trait, `Arc`/`Mutex` not needed since the cache is single-owner, LRU via
  `VecDeque`, capacity-bounded eviction).

Evidence value: the fat kimi image's Rust toolchain works, module splits round
trip, injected-clock TTL is tested without sleeping, and the answer carries the
complete source and test summary.

### kimi — task B: Go worker pool + token-bucket rate limiter + race detector

Job `620f3fed…` (`external:kimi`, `completed`, 462 s). Correctness verified
independently from the reported JSON:

```json
{"total":200,"succeeded":171,"permanently_failed":29,"total_attempts":287,
 "duration_ms":2892,"throughput_per_sec":69.15,"rate_limit_ok":true}
```

- `29` jobs with `i % 7 == 0` in `0..200` is exactly right (there are 29 such
  indices), each taking 4 attempts (1 + 3 retries) = `116`, plus `171`
  single-attempt successes = `287` total attempts — matches verbatim.
- `2892 ms >= 2870 ms` (the `287/100` s floor), so `rate_limit_ok` is truthful,
  and the run was under `go run -race .` which reported **no data races**.
- `go version go1.24.3 linux/amd64`; full `main.go` returned.

A second concurrent dispatch of the same task (`93b837ac…`) was rejected in
`0 s` with `Workspace /workspace is already being modified by task 620f3fed…
Cancel or finish that task before dispatching another task to this workspace.`
— the workspace-overlap exclusivity guard (the exact item-101 NW-1 concern)
working as designed: two jobs resolving to the same `/workspace` cannot
coexist, and the second is refused cleanly rather than corrupting the first.

### pebble — task C: Dijkstra (C++17, heap vs brute-force + stress)

Job `a938edb3…` (`external:pebble`, `completed`, 152 s).

- `g++ (Debian 12.2.0-14+deb12u1) 12.2.0`, `-O2 -std=c++17 -Wall -Wextra`,
  **zero compiler warnings**.
- Test graph output was `0 5 9 / 0 3 4 / 0 4 7 / 5 0 unreachable / 2 5 8`,
  and the worker *independently flagged that the prompt's* `0->5 = 7`
  expectation was wrong, computing `0→2→1→3→4→5 = 9` — an integer-arithmetic
  sanity check that the result is computed, not sycophantically echoed.
- `STRESS_OK all_pairs=40000` — the heap and O(V²) implementations agreed on
  all 200×200 pairs of a 200-node/2000-edge random graph.

Evidence value: the fat pebble image's C++ toolchain works, `std::priority_queue`
Dijkstra is proven correct against an independent brute force on 40,000 pairs,
and the worker caught a genuine error in the (autonomous-agent-authored) task
prompt rather than reproducing it.

### pebble — task D: generic ring buffer with iterators + `Summarize` trait (Rust)

Job `9a6c9aed…` (`external:pebble`, `completed`, 121 s).

- `rustc 1.96.1` / `cargo 1.96.1`; `test result: ok. 12 passed; 0 failed`.
- `summarize()` on the wrapped buffer holding the last 8 of `1..=100` returned
  `n=8 sum=772 min=93 max=100` — `sum(93..=100) = 772`, arithmetically exact,
  proving the 1000-push/16-capacity churn retained exactly the last 8 values in
  order.
- Full `src/ring.rs` returned, with safe `MaybeUninit` handling, wrap-aware
  `phys_index`, `into_iter`/`Iter`, and a degenerate zero-capacity guard.

Evidence value: the pebble lane's Rust toolchain works (same 1.96.1 pin), the
generics + iterators + trait constraints all compile and test, and the wrap
arithmetic is verified by the exact-sum check.

## Landed-changes exercise matrix

| Area | Evidence on mistyfly |
| --- | --- |
| kimi worker lane | fat image, kimi-code **2.3.0**, `job_mode=external:kimi`; two real jobs completed |
| pebble worker lane | fat image, pebble **0.5.0** (`pebble-worker-0.1.0` bridge), `openai/gpt-5.2`; two real jobs completed after the F5 egress fix |
| Full toolchains (the item's explicit ask) | kimi image: rustc 1.96.1, go 1.24.3 — both exercised in-container. pebble image: g++ 12.2.0, rustc 1.96.1 — both exercised in-container |
| WI#357 workers never self-expose | `worker-listeners mistyfly` → `clean` |
| WI#424 egress enforcement | first fresh tenant on this host to allocate a subnet; both workers `attached (exclusive, DNS pinned, static hosts current)` |
| WI#330 no-default LLM endpoint | `KIMI_BASE_URL_SOURCE=inherited`; LLM endpoint inherited, jobs called it successfully |
| WI#472 approval thread resolution | 404 (no gate) / 400 (wrong thread) verified live |
| WI#471 pebble key quoting | all four rejection arms verified live, no `pebble.env` written |
| Knowledge seed + sweep + study MCP | 1795 notes seeded / 20 failed; `study_notes` MCP active; sweep ran (F4 documents the refresh failure) |
| WASM channel/tool suite | 3 channels + 9 tools; 13 extensions total |
| Workspace-overlap exclusivity | second concurrent kimi dispatch refused with the exact ownership error |

## Finding → owner index

QQ-4 asked for the four target-side findings to be cited in their headings so
they stay traceable once the tenant is gone. Filed at the maintainer's request
after run 250 and confirmed open against this tree:

| finding | work item | severity label | one-line mechanism |
| --- | --- | --- | --- |
| F1 — worker source-root asymmetry | [WI#507](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/507) | medium, mt-admin / tech-debt | `build_pebble_worker` takes no tenant source root, so tenant pebble images build from the operator's live checkout and stamp its moving, possibly dirty revision |
| F2 — egress apply fail-closed deadlock | [WI#504](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/504) | high, mt-admin / security | originally separate unshare sessions plus an unpinned namespace; item 114 adds the pause keeper and shared single-session guard, verified with the live cold-start fixture described in F2 above; full tenant deployment remains an operator check |
| F2b — orphaned tenant lifecycle lock | [WI#505](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/505) | high, mt-admin / reliability | the lock descriptor is inherited by OpenRC daemons started inside the locked section, so a `die()` leaves the flock held and every later lifecycle command blocks |
| F5 — no pebble backend in the allowlist | [WI#506](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/506) | medium, mt-admin / network | a `--with-pebble` tenant has no allowlist entry for pebble's model backend, so every pebble job fails on an egress-enforcing host |
| F2a — launch-args status bug | **fixed on this branch** (`64442bffa`) | — | the IP-only allowlist made `worker_egress_launch_args` return 1 after printing valid args |
| F3 — prior-pass findings (numbering is thunderlane's, not this report's) | WI#471 / WI#472 / WI#473 fixed, WI#436 item 1 still open | — | three of four named prior findings landed and are regression-verified live on mistyfly |
| F4 — knowledge-sync seed/refresh asymmetry | WI#436 item 1 | — | one bad corpus note still poisons the whole refresh |

F2a was closed by the original item-99 MR. Item 114 supplies the F2 fix and
isolated live evidence above; WI#504 stays open for maintainer acceptance. F1,
F2b and F5 remain owned by their separate upstream work items.

## Disposition

The tenant is **left running** (all services boot-persistent under OpenRC) for
maintainer inspection:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh status mistyfly
sudo -u mistyfly bash -c 'XDG_RUNTIME_DIR=/run/user/$(id -u mistyfly) podman ps'
```

`mistyfly` is not an animal name and comes from the WI#273 list, so the
teardown routine must not target it. To decommission later anyway:
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant mistyfly --purge`.

One operational note for whoever later decommissions: the F4 knowledge refresh
is in permanent backoff (`consecutive_failures` climbing), because the shared
corpus still carries the raw-NUL note; that does not block the tenant but means
it serves a stale corpus until WI#436 item 1 lands upstream.
