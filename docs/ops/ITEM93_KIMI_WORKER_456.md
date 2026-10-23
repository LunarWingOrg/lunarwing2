# Item 93 — WI#456 kimi external worker `connection refused`: investigation, fixes, and temporary-tenant verification

> Investigated 2026-10-22 by SweetieBot (autonomous), branch
> `sweetiebot/kimi-93-20261022-1828`. Companion work items (all referenced
> from WI#456's description): [WI#461](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/461),
> [WI#462](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/462),
> [WI#463](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/463).
> The temporary test tenant used for verification is `kimi93test` on the
> Gentoo dev VM (see below) and is **torn down** at the end of the run.

## The symptom (WI#456)

Dispatched kimi canary jobs failed with:

```
External worker 'kimi' connection failed: IO error: Connection refused (os error 111)
```

The daemon reported `External workers configured: kimi, pebble, opencode` at
boot (the `config.toml` route exists), but there were **no kimi-specific ERROR
lines** anywhere in the daemon log — the failure only surfaced at dispatch
time, connecting to a dead socket. Same signature as the pebble worker issue
(WI#452, closed as "rebuild needed").

## Root cause (verified live on the Gentoo dev VM)

The kimi lane regressed on the new build because **stale tenant-store kimi
images predate the WI#400 root-bridge shape**. Concretely, on this host:

- Tenant `scootaloo`'s rootless podman store carried
  `lunarwing-worker-kimi:latest` built 2026-10-16 with **`USER 10002:10002`**
  (pre-WI#400), while the current tree pins **`USER 0:0`**
  (`kimicode4lunarwing/Dockerfile:269`, image env `KIMI_TASK_UID/GID=10002`,
  entrypoint running the root bridge and dropping tasks via setpriv).
- The rootful admin store had **no** kimi image at all, so
  `_ensure_tenant_image`'s stale-accept branch
  (`[[ -n "$tenant_id" ]] && return 0`) silently kept the tenant's old copy
  in play.
- On the next `start-tenant`/worker start, mt-admin's image-user guard
  (`ic/scripts/mt-admin-lib/09-worker-config.sh:2170`) then `die`s:
  `kimi worker image has user '<actual>', expected 0:0 … rebuild it`. That
  die is **deliberate** (fail loudly rather than silently run a pre-WI#400
  bridge as the task uid — introduced with the WI#400 lanes, commit
  `79a60ee22`).
- The failure class is exactly WI#456's: the worker never starts (or the
  already-running stale container wedges), the daemon-side
  `[[sandbox.external_workers]]` route stays hot, and the next
  `create_job(mode: "kimi")` dials `ws://127.0.0.1:<kimi_wss>/ws/agent`
  with nothing listening → **os error 111, no kimi lines in daemon.log**.

WI#452's pebble note is the same class with one difference: pebble's image
carried `USER unset`; kimi's stale copies carry the *old task uid*.

## Fixes landed in this item

1. **The remediation itself — rebuild the image.**
   `build-kimi-worker` on the current tree produces `USER 0:0`
   (verified: `podman inspect lunarwing-worker-kimi:latest --format
   '{{.Config.User}}'` → `0:0`). Once the admin store holds a rebuilt
   image, `_ensure_tenant_image` distributes it to tenant stores (the
   stale-accept branch only applies while the admin store has *no* copy) and
   the launch contract (`_worker_launch_contract` pins the tenant-store
   image ID) force-recreates the running container on the next start — the
   lane is self-healing after a rebuild. **Operators hitting WI#456: run
   `sudo bash ic/scripts/lunarwing-mt-admin.sh build-kimi-worker`, then
   `start-tenant <name>` (or `start-worker <name> kimi`).**

2. **WI#461 (LOW, assigned): `prepare_worker_workspace` chown walk unchecked.**
   Both chown steps of the rootful workspace handoff
   (`09-worker-config.sh:1989-2005`) ran bare under `set -euo pipefail`.
   In a direct call context a failure aborted the whole mt-admin run from
   inside the function; in a suppressed-errexit context
   (`cmd || status=$?` — the shape the daemon-facing callers use) the same
   failure was **silently swallowed** and the function returned 0 with the
   bind mount never handed to the container uid. (The long-standing single
   MI-3 harness failure on the noexec-/tmp dev VM was exactly this silent
   swallow, live.) Both steps are now `if !`-guarded with a clean rc-1
   refusal, PWD restore on every exit path (MV-4), and two mutation-verified
   pins in `test-mt-admin-kimi.sh`. Harness now 140/140.

3. **WI#462 (LOW, assigned): task-uid PATH ordering — REORDERED per run-195 review (NB-10).**
   The executor's task toolchain PATH
   (`kimicode4lunarwing/scripts/kimi_task_executor.ts`) now appends the
   task-uid-writable dirs AFTER the ME-1-pinned root-owned dirs — the same
   posture as the bridge PATH. The original task-first ordering (and its
   "task could shadow root's binaries" rationale) was backwards: task-first
   is what lets one task's leftovers (a planted `node`, a rewritten entry
   point) become the tools every LATER task resolves — including the kimi
   CLI's own env-node interpreter lookup. Root-first ordering keeps the
   task toolchain working (names root does not ship still resolve from the
   task dirs) while removing the cross-task shadowing residual. A new pin
   asserts the root-first order in the recorded child env (mutation-checked).

4. **WI#463 (INFO, assigned): bare-name `kimi` resolution at the drop — FAIL-CLOSED per run-195 review (NB-11).**
   `kimiBin()` returned a bare `kimi` that the *task-side* PATH would resolve
   after the setpriv drop. New `taskKimiBin()`: a bare name resolves through
   the **bridge's own PATH** (root-ownership-checked via
   `resolveFromBridgePath` when the resolver runs as root — the same
   treatment setpriv gets, WI#400 LD-11); an absolute `KIMI_BIN` (every
   operator/test override) passes through unchanged. **Amended by NB-11:**
   the original catch-and-fallback returned the bare name when the bridge
   lookup missed — sending setpriv down the child PATH, the lookup WI#463
   exists to avoid. The pinned path now FAILS the task when the bridge
   lookup fails (mutation-verified: restoring the fallback fails the pin).

## Temporary test tenant verification (`kimi93test` — superseded by the full stale-image reproduction, 2026-10-22 run-195 review)

The first verification pass (tenant `kimi93test` on the Gentoo dev VM, torn
down after use) established that a freshly-built image boots, hands over the
WS handshake with the scoped bearer, and reports ready — but it never
reproduced the FAILURE (the tenant was created fresh with the rebuilt image,
so there was no stale image to heal; NB-7). The full failure→fix chain was
reproduced afterwards on this host (Sigma, kernel 6.18.54-gentoo); see the
next section. WI#456 itself was reported on a Fedora host (kernel
7.2.9-100.fc43.x86_64 per the WI body — a different box from both
verification hosts; the stale-image class is host-independent because it
lives in mt-admin's image-distribution logic, not in any host kernel).

### What the first pass DID establish (still valid)

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh add-tenant kimi93test \
  --with-kimi \
  --tensorzero-url http://192.168.1.157:3000 \
  --kimi-base-url http://192.168.1.157:3000/openai/v1 \
  --no-ssh
sudo bash ic/scripts/lunarwing-mt-admin.sh start-tenant kimi93test
```

- Image `USER 0:0`, container root with the setpriv task drop
  (`KIMI_TASK_UID/GID=10002`), `kimi worker ready`.
- Daemon boot logged `External workers configured: kimi, pebble, opencode`.
- WS upgrade with `Sec-WebSocket-Protocol: lunarwing-agent-v1` + scoped
  bearer → **101**; wrong bearer → 401. `/ready` → true.
- LLM-driven `create_job` attempt failed at the LLM upstream (dev VM's
  TensorZero has no model-provider credentials — deploy-gated, same class
  as the item-69/!1783 acceptance evidence).

## Full failure→fix reproduction (NB-7 remediation, Sigma host, 2026-10-22)

The reviewer's recipe, executed end-to-end with a temporary tenant
(`dingo`, registry `source: autonomous-test`, purged after):

**Seed (the BEFORE state):**
1. `add-tenant dingo --with-kimi` (fresh tenant, no start yet).
2. The tenant's rootless store was seeded with the ACTUAL stale pre-WI#400
   artifact: `lunarwing-worker-kimi:latest` image `13da3b7dad6f`
   (`USER 10002:10002`, built 2026-10-16) saved out of tenant `scootaloo`'s
   store — the same image class the original diagnosis found — and loaded
   into dingo's store under the same tag.
3. The admin store's kimi image was untagged (saved to tar first), arming
   `_ensure_tenant_image`'s stale-accept branch: tenant holds an image, the
   admin store has none → the tenant's stale copy stays in play.

**Stage 1 — the failure, captured live:**
4. `start-tenant dingo` → `start_tenant_kimi`'s image-user guard **died
   exactly as diagnosed**:
   `error: kimi worker image has user '10002:10002', expected 0:0 (WI#400
   root entrypoint; tasks drop to 10002); rebuild it`
5. The kimi container was never created; the daemon-side config route
   (`ws://127.0.0.1:30030/ws/agent`, rendered at add-tenant) stayed hot
   against a socket with **nothing listening** — dialing it returned
   **connection refused (os error 111)**, the exact WI#456 signature.
6. The tenant services that had already started (pg/vision/weechat/xmpp/
   daemon) kept running — WI#456's "no kimi ERROR lines, dead socket at
   dispatch" shape reproduced in full.

**Stage 2 — the fix, captured live:**
7. The admin store's rebuilt image (`562a224f491b`, `USER 0:0`, revision
   `87dc5bc98`) was restored.
8. `start-tenant dingo` again → mt-admin **detected the stale image and
   refreshed it** (`refreshing stale image lunarwing-worker-kimi:latest in
   dingo's rootless store`), distributed the new image (`dingo` store
   inspected: `USER=0:0`, ID `562a224f…`), **force-created the container on
   the new image ID**, and reported `kimi worker ready (lunarwing-kimi-dingo,
   WSS port 30030)`.
9. WS upgrade with the scoped bearer → **HTTP/1.1 101 Switching
   Protocols**; `/ready` → `ready: true, websocket.listening: true`. The
   daemon booted with `External workers configured: pebble, kimi, opencode`.
10. The dead socket became a live one **without any manual container
    surgery** — the "image distribution + image-ID launch contract
    self-heal" claim is now demonstrated, not just argued.

**Teardown:** `remove-tenant dingo --purge` (tenant, user, ports block,
containers removed; boot cgroup file re-rendered with the survivors).

**Scootaloo (the originally-diagnosed stale-image holder) was left as
found, per NB-7's note — its next `start-tenant` picks up the rebuilt
image via the same demonstrated path.** Tenants known to hold stale kimi
images at the time of writing: `scootaloo` (13da3b7, now demonstrably
self-healing).

## What this item does NOT change

- The image-user guard's `die` stays (deliberate, WI#400); this item makes
  sure a rebuilt image exists and documents the one-command remediation.
- `_ensure_tenant_image`'s stale-accept branch (admin store empty → tenant's
  copy wins) is unchanged: the image-user guard is the loud backstop, and
  changing the accept rule would break offline fleets where only a tenant
  store holds the image.
- The daemon's `ExternalWorkerConnectionFailed` path is unchanged — with a
  healthy worker the connection succeeds (verified 101).

## Verification summary (updated for the run-195 review fixes)

- `ic/scripts/tests/test-mt-admin-kimi.sh`: **144/144** (140 before the
  review round; +2 NB-9 walk-guard pins, +1 stub-exec guard, +1 fixture
  cleanup line accounted for in the pass count).
- **NB-8 correction (MI-3 causal claim):** the reviewer reproduced the
  dev-VM MI-3 failure with a noexec tempdir and showed it is a TEST-
  ENVIRONMENT artifact, not the WI#461 hole — the stub cannot execute from
  a noexec directory, so `find -exec` falls through to the real `chown`
  and the walk assertion grades the environment. This head reproduces the
  reviewer's numbers exactly (138→139 PASS / 2 FAIL under noexec, full
  pass with an exec-able tempdir). The earlier claim that "the MI-3
  failure was the WI#461 hole live" is RETRACTED; the WI#461 guard remains
  valuable for its actual purpose (clean refusals + PWD restore), and the
  harness stub-derived pins now SELF-SKIP with a named reason when the
  fixture tempdir cannot execute stubs, so the environment never decides
  a code verdict again.
- kimi TS suite (`npm test` under `kimicode4lunarwing/scripts`):
  **21/21** in the executor lane (19 + the NB-10 order pin + the NB-11
  fail-closed pin); full suite green non-root, documented real-CLI skips
  root-shaped.
- Mutation checks (review round): WI#461 bare-chown revert fails the rc-1
  pin; walk-guard removal fails exactly the NB-9 pin (2 FAIL, restored);
  WI#462 task-first order revert fails the NB-10 order pin; WI#463
  fallback restore fails exactly the NB-11 pin; WI#463
  `taskKimiBin()`→`kimiBin()` revert fails exactly the bare-name argv pin.
- `check-mt-admin.sh` shellcheck clean (16 lib files); repo `shellcheck.sh`
  clean; `shell-syntax.sh` 255 scripts + 3 hooks clean; `tsc` clean.
- Full failure→fix reproduction on a temporary tenant (NB-7): see the
  dedicated section above — guard `die` captured, os-111 dead socket
  captured, image refresh + force-recreate + live 101 captured, tenant
  purged.
