# Item 100 (v2.1.7.0) — persistent test tenant `windrider`

## Scope and status

This is the v2.1.7.0 item-100 pass on branch
`rarity/kimi-100-20261029-1936`, continuing the earlier
`rarity/kimi-100-20261029-1200` pass (merged as `98118fdad`).
Started 2026-10-29 by Rarity. No subagents are used.

The deliverable is a **new** tenant with kimi and pebble external workers,
full toolchains, complex tasks executed through the real worker dispatch path,
and independently checked results. The tenant must remain running afterwards
and must not be torn down.

`windrider` is a non-animal name from the project's
[WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
test-tenant list, so it is not matched by the routine animal-name teardown.
This pass does not reuse item 99's tenants (`mistyfly`, `thunderlane`) or
item 101's (`highwinds`, `silverzoom`) or their results.

## What the earlier 1200 pass landed

That pass provisioned `windrider` and fixed the **rootless egress keeper**
blocker that stopped item 101 on a Podman 6.1.4 host. On this host podman is
5.8.2 and the built-in infra container is image-less: `ImageName == ""`,
`IsInfra=true`, `Path=/catatonit`, `Args=["-P"]`, local generated rootfs.
The pre-existing guard accepted only `localhost/podman-pause:<tag>`, so every
standard `start-worker` failed closed before it could start its replacement —
exactly the item-101 hand-off failure.

Commits already in this branch's history:

- `c7438ad75` identify Podman native pause keeper metadata
- `fb502652d` accept constrained image-less Podman namespace keepers
- `72f589f6d` model native pause metadata in egress fixtures
- `1dc0a3838` cover native keeper acceptance and hostile metadata

`ic/scripts/lunarwing-worker-egress-guard.sh` now falls back to
`keeper_uses_native_pause()` when `ImageName` is empty. That fallback is
**shape-pinned, not trust-based**: it requires the exact tuple
`""|/catatonit|["-P"]|[]|/run/user/<uid>/libpod/tmp/infra-container`,
including an empty mount list and the per-uid generated rootfs path, so an
arbitrary image-less or mounted-rootfs container is still removed and the
start refused. A non-empty `ImageName` still has to match
`localhost/podman-pause:*`. The label, container-count, `IsInfra` and
exclusive-network checks are unchanged.

What the 1200 pass did **not** do: build worker images, start the workers, or
run any complex task. Its record explicitly claimed no build, no worker task
and no completion. This pass is that work.

## Host and procedure

- Operator checkout: `/home/rarity/lunarwing2_gitlab` on the dev VM.
- Host: Linux/OpenRC, rootless podman **5.8.2**, 18 CPUs, 17 GiB RAM,
  ~85 GB free on the shared `/` (which holds `/home`, `/var` and `/tmp`)
  before provisioning.
- Tenant uid **1004**; allocated block base_port 10010 — gateway 10010,
  http 10011, postgres 10013, orchestrator 10016, pebble_wss 10018,
  kimi_wss 30010, kimi_health 30011, pebble_health 20004,
  vision_service/health 20015/20016.
- Pre-existing state found at the start of this pass: tenant user, port
  allocation, cloned checkout and generated env all present; PostgreSQL
  container running; daemon release binary already built
  (`/home/windrider/lunarwing/ic/target/release/lunarwing`, 111 MB);
  **no** WASM extensions installed, **no** worker images, daemon stopped.
- All lifecycle operations go through `ic/scripts/lunarwing-mt-admin.sh`
  (root). No bare `systemctl`/`rc-service` against tenant units from
  wrappers, per the AGENTS.md multi-tenant hard rule.
- Long builds run in tmux, bounded to 18 CPUs. No full debug daemon build is
  used.
- Credentials come from the existing process environment and are passed to
  supported configuration commands without printing or committing them.
- Logs are kept under `/tmp/lw-item100/` on this host.

## Plan

1. Start the tenant daemon and confirm the gateway answers.
2. Build the kimi and pebble worker images with `--with-toolchains`, then
   install WASM tools into the tenant state dir.
3. Start both workers through mt-admin, configure and audit the per-tenant
   egress allowlist and listener exposure.
4. Dispatch complex tasks to each worker through the real gateway dispatch
   path (`create_job`), one lane at a time, into distinct workspace
   subdirectories.
5. Verify each artifact against an **independent** oracle on the host — never
   by re-asking the worker that produced it.
6. Record findings, image provenance, ports, job IDs and final live health
   here. Leave the tenant running; no teardown.

No secret inspection, no unrelated project changes, no subagents.

## Daemon start and LLM path

`start-tenant windrider` brought PostgreSQL, the daemon, weechat and the WS
adapter up. The XMPP bridge failed to start
(`ic/bridges/xmpp-bridge/target/release/xmpp-bridge is not readable` — the
binary was never built for this tenant); XMPP is unrelated to worker dispatch,
and `build-tenant --with-wasm` rebuilds it, so this is recorded rather than
worked around.

- Gateway `:10010/api/health` → `{"status":"healthy","channel":"gateway"}`.
- Daemon `lunarwing --version` → **2.1.7**.
- Bearer token read via `mt-admin tokens windrider` at runtime; never printed
  or committed.
- **Agent LLM loop proven:** a plain gateway turn answered exactly `BOOT_OK`
  in ~8 s with thread state `Completed`, so the tenant's own LLM path works
  before any worker is involved.
- Credentials were already present in the tenant env at provisioning time
  (`LLM_API_KEY`, `KIMI_MODEL`, inherited `KIMI_BASE_URL` with
  `KIMI_BASE_URL_SOURCE=inherited`, and `pebble.env` carrying
  `NANOGPT_API_KEY`/`PEBBLE_MODEL`). Verified present and non-empty by key
  name only, with values redacted.

## Worker image builds (both fat / `--with-toolchains`)

Both built through standard mt-admin in tmux, exit 0, no full debug daemon
build anywhere.

| lane | image id | size | source revision | toolchain profile |
| --- | --- | --- | --- | --- |
| pebble | `5813f4adecc367c19d7cb86773ddf33b083a849618140baa48932c1a2a30e9c2` | 2,354,485,090 B | `05688d65b09a8baed4879da0398fec96d46da7bc` | true |
| kimi | `6d6cef28d720d7e554465c00843d63858bc0c326ec98fd37839a60e38acf69ac` | 2,737,280,633 B | `f84999b145936b2208e0cc288ccce365c1a74317` | true |

**Provenance caveat, stated plainly (the WI#507 class):** the two images carry
*different* source revisions. `f84999b14` is this pass's own findings-doc
commit, made between the two builds, so `SOURCE_REVISION` differs although no
worker source changed. Both builds used the operator checkout
(`/home/rarity/lunarwing2_gitlab`), not the tenant clone — unlike item 101,
where pebble used the operator checkout and kimi the tenant clone.

- Pebble passed the **WI#334 toolchain smoke** in-build, on both legs — the
  real `/bin/sh -lc` task shell *and* a fully sanitised `env -i /bin/sh -c`:
  `cargo 1.96.1 (356927216 2026-06-26)`, `rustc 1.96.1 (31fca3adb 2026-06-26)`,
  `go version go1.24.3 linux/amd64`, `rustup 1.28.2`. Final line
  `smoke: PASS`.
- Kimi ran its in-image suite: **186 tests, 183 pass, 0 fail, 3 skipped** (the
  runtime-image leg; the earlier multi-arch leg reported 179 pass / 7 skipped
  before the toolchain stage), including the real-CLI contract lane.
- Rust in the kimi image installs from the SHA-256-pinned rustup archive
  (`RUSTUP_SHA` checked with `sha256sum -c` before execution) with
  `--default-toolchain 1.96.1`.
- Debian packages come from the pinned snapshot
  `snapshot.debian.org/archive/debian/20261024T011300Z`, not a live mirror.
- Logs: `/tmp/lw-item100/{pebble,kimi}-build.log`.

## Egress-guard regression evidence (the enabler this pass depends on)

The rootless worker egress guard refuses any keeper infra container whose
metadata is not exactly the Podman native image-less pause shape. `test-worker-egress.sh`
ran green on this tree: **144 PASS / 1 SKIP** (the skip is the opt-in live-
acceptance arm, gated on `LUNARWING_MT_EGRESS_LIVE=1`). The Native-pause cases
specifically:

- PASS — guard accepts the Podman native image-less pause keeper.
- PASS ×5 — guard rejects unexpected native pause metadata: `sha256:foreign|…`,
  `|/bin/sh|…`, `|/catatonit|["-P","extra"]|…`, a non-empty mount list `|[{}]|…`,
  and a foreign rootfs `/tmp/foreign-rootfs`. Each also proves the invalid
  keeper is rejected **before** any `nft` load (no filter is ever bypassed).

This is the exact guard that item 101 (on Podman 6.1.4) reported as the blocker;
on this host podman is 5.8.2 and the fallback is exercised when a worker starts.

**Live confirmation on this host (not just the fixture):** after `start-worker`
succeeded, windrider's keeper pod `lw-egress-keeper-windrider` has infra
container `0fd45486c44f6c9b4b374e76bc4c3800834aaffbd14bf2966acf38523f69dcc7`
with `ImageName` **empty**, `Path=/catatonit`, `IsInfra=true` — precisely the
shape `keeper_uses_native_pause()` accepts. So the image-less built-in infra
container is **podman's default on 5.8.2 as well**, not a Podman 6.1.4 quirk as
item 101's report assumed; without the `c7438ad75`/`fb502652d` fallback this
tenant's workers could not have started either.

## Independent-oracle methodology (how "verified" is defined here)

Item 100 says *extensive tests*, so this pass treats a result as verified only
when a **host-side** implementation reproduces it. Concretely:

1. Each task prompt requires the worker to **dump machine-readable artifacts**
   (`ops.txt`, `instances.txt`, `data.csv`, `RESULTS.json`) describing the exact
   inputs it solved — not just its own conclusions.
2. `/tmp/lw-item100/oracles.py` re-derives every answer with an independent
   Python implementation: naive replay for the Fenwick range-add/range-sum,
   exhaustive s-t cut enumeration for max-flow, brute-force enumeration of all
   injective maps for the assignment problem, and a stable-sort + inner-join
   model for the CSV lane. It compares against the worker's reported numbers
   and the worker's own in-container brute force.
3. Artifacts are pulled out of the worker container with `podman cp`/`exec` and
   checked on the host, so the worker cannot grade its own homework.
4. **Every oracle was self-tested before use** — first on synthetic data to
   prove it accepts a correct result, then against deliberately mutated inputs
   (checksum off by one, one instance's flow bumped, one assignment optimum
   bumped, a swapped sort row, a dropped join row). All six mutants were
   rejected with a precise message. An oracle that only ever says OK would be
   worthless; these do not.

The six lanes (2 workers × 3 languages) are the item-101 plan, kept because
item 101's own report never got to run them:

| Lane | Task | Host-side oracle |
| --- | --- | --- |
| Kimi / Rust | Two-tree Fenwick inclusive range add/sum, signed i128 | Naive per-position replay of `ops.txt` |
| Kimi / Go | Concurrent snapshot-isolation transactional store | `go test -race` plus host-run stress assertions |
| Kimi / C++17 | Dinic max flow, parallel edges and residuals | Exhaustive small-graph min-cut enumeration |
| Pebble / Rust | Segment tree + rollback DSU connectivity | Host naive `Vec` scan and root-set comparison |
| Pebble / Go | External stable CSV sort + merge join | Host stable-sort and inner-join model over the dumped CSVs |
| Pebble / C++17 | Rectangular Hungarian assignment | Brute force over all injective maps + dual-certificate check |

Per item 101's F12 (task *size* mattered more than the lane there), these are
six focused jobs rather than one oversized one, dispatched serially per lane.

## New blocker found and fixed this pass — podman `inspect` prefix cross-match

`start-worker windrider pebble` failed closed at the first attempt. Root cause,
isolated with `bash -x` and a live podman probe:

- The pebble **bridge container** is named `lunarwing-pebble-windrider`; its
  IPC **volume** is `lunarwing-pebble-windrider-ipc-v2`.
- `podman inspect <name>` (and docker's) resolves a name across **containers,
  images, volumes, networks and pods**, and matches a name that is only a
  **prefix** of another object. With no bridge container yet present, a generic
  `inspect lunarwing-pebble-windrider` resolved to the *volume* and returned 0.
- The existence gate `if ! _ctr inspect "$container_name"` therefore read
  "container exists", **skipped container creation**, then
  `_register_worker_unit` → `worker_egress_verify_after_start` failed closed on a
  container that was never created, stopping the worker.
- `podman container inspect lunarwing-pebble-windrider` correctly reports
  "no such container" (rc 125), and `podman rm -f lunarwing-pebble-windrider`
  does **not** match-remove the volume (proven with a throwaway volume), so the
  reconcile path could never corrupt the IPC volume — the bug is solely a
  false-positive existence check.

Fix: added `_ctr_inspect` in `ic/scripts/mt-admin-lib/02-containers.sh` that runs
the portable typed subcommand `container inspect`, and routed all 82 container
existence/state/health checks in `mt-admin-lib` through it. This is a podman 5.8.2
OpenRC-path bug — on systemd hosts the Quadlet path does not hit these imperative
`inspect` gates (which is why item 101's systemd silverzoom never saw it). The
affected mt-admin fixtures were updated to model the typed subcommand; the worker,
vision, voice, image-contract, lifecycle, startup, upgrade and pebble-configure
test suites all pass on this tree.

## Blocker that remains — gateway model does not follow the CodeAct contract

Both workers run healthy, but a **real `create_job` dispatch through the gateway
LLM loop never fires**. Evidence (daemon run at `debug` log level):

- A plain `Reply with exactly BOOT_OK` and `What is 2+2` turn both complete
  fine, so the model answers text normally.
- A delegation turn — even the preamble's own documented exception form, a bare
  `create_job(title=..., mode="kimi", description=...)` structured call — yields
  `response: null`, no `tool_calls`, and the engine logs
  `NOTE: 1 text response(s) without code blocks — model may not be following
  CodeAct prompt` then `Suppressed empty response (not sent to channel)`.

The tenant's gateway LLM is `LLM_MODEL=openai/gpt-5.2` pointed **directly** at
`https://nano-gpt.com/api/v1` (no TensorZero proxy). That model does not honour
the CodeAct ` ```repl ` / structured-tool contract the engine (v2) expects, so it
never emits the `create_job` tool call that is the only way a job is created
(`/api/jobs` is read-only; jobs are created solely by the agent's `create_job`
tool). The working sibling tenant `dash` uses `LLM_MODEL=…tensorzero::function_name::lunarwing`
(the local TensorZero proxy), which is a CodeAct-following surface.

This is a **tenant-configuration** blocker, not a worker or daemon defect. The
workers themselves are proven healthy on their published ports (pebble `:20014`,
kimi `:30011`, both `{"status":"ok"}`), the egress filter is live, and the
listener audit is clean — the gap is purely that this tenant's gateway model
cannot drive `create_job`. Fixing it means re-pointing the tenant at a CodeAct
LLM (the TensorZero `lunarwing` function), which is an operator configuration
decision, not something this pass silently rewrites.

## Live health at hand-off

- Tenant `windrider` is **left running**: PostgreSQL, daemon
  (`lunarwing 2.1.7`), weechat + adapter, and both workers
  (`lunarwing-pebble-windrider` `:10018`/`:20014`,
  `lunarwing-kimi-windrider` `:30010`/`:30011`) all `started`, boot-persistent.
- WASM: 12 tools/channels installed, 1 skipped (not built).
- `worker-egress windrider` exit 0 — 4 containers exclusively on
  `lw-egress-windrider`, DNS pinned to 127.0.0.1, static host
  `nano-gpt.com:216.150.1.1`, only `tcp dport 443` allowed, all else reject.
- `worker-listeners windrider` → `worker listener audit: clean` (host loopback
  only; no worker self-exposure).
- `worker-smoke windrider` was not run to completion: it first demands the
  non-selected opencode2 worker. Kimi/pebble bridge health was instead checked
  directly (both `{"status":"ok"}`).
- Verification harness lives in `/tmp/lw-item100/` (`lw.py`, `dispatch.py`,
  `verify.py`, `oracles.py`, six task prompts, build logs).

## Status and STOP report

**Item 100 is not fully complete.** The tenant, toolchains and both workers are
up, permanently named `windrider` (non-animal, from WI#273), and left running
with no teardown — the bulk of the item. Two findings were produced and the code
fix committed. What is **not** done is the complex-task dispatch, because the
tenant's gateway model (`openai/gpt-5.2` via nano-gpt.com, no TensorZero proxy)
does not follow the CodeAct contract, so `create_job` never fires and no job can
be created through the real path.

**STOP / TRIED / BLOCKED BY / RECOMMEND**
- STOP: 8 attempts to produce a `create_job` through the gateway LLM loop all
  yielded `response: null` with the engine logging "model may not be following
  CodeAct prompt"; the same shape failed for a trivial smoke and for the
  preamble's own documented structured-call exception form. Identical-root-cause
  repeats, not a new error each time.
- TRIED: CodeAct-friendly and structured-form prompts; raised log level to
  `debug` to capture the real reason; confirmed the workers are healthy on their
  ports and the egress/listener audits are clean.
- BLOCKED BY: `windrider`'s gateway LLM is `openai/gpt-5.2` pointed straight at
  `nano-gpt.com/api/v1` with no TensorZero proxy; that model does not emit the
  `create_job` tool call the engine requires to create a job.
- RECOMMEND: re-point the tenant's gateway LLM at a CodeAct-following surface
  (the TensorZero `lunarwing` function, as `dash` uses), then run the six
  prepared tasks through `dispatch.py` and verify with `verify.py`/`oracles.py`.
  This is an operator config decision I will not make silently.

The item checkbox stays `[ ]`. Tenant and ports retained; no teardown.
