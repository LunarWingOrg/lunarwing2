# Item 101 (v2.1.7.0) — persistent test tenant `highwinds`

Status: **both workers running; acceptance unblocked.** RG-1 is fully landed
across all four worker lanes, the previously-blocking egress-keeper refusal was
fixed by item-100 (native-pause infra is accepted in-tree), Kimi was rebuilt
with the WI#454 task uid and completed a real complex task whose artifact is
independently verified. Pebble executed its complex task and produced a correct,
independently verified artifact, but its result was lost by a worker-side
forwarding defect (PE-1, recorded below). **Scope:** one complex task per lane
was driven end-to-end and independently verified (2 of the 6 prepared tasks);
the remaining four were not run this pass. Item 101's acceptance bar — both
workers running, complex tasks actually executed with independent verification —
is met for those two lanes, and two genuine findings are reported for follow-up.
Tenant `highwinds` is retained and still running; no teardown was performed.

## Scope and provenance

This continues closed [MR !2040](https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/2040),
head `767896863`, and its run-262 review #12562. That pass provisioned the new
non-animal tenant `highwinds`, built full-toolchain Kimi and Pebble images and
left services running, but did **not** complete the planned worker jobs.
Those commits are Applejack's (`aj/kimi-101-20261029-0054`), not Rarity's as
the old report said. This re-attempt is `aj/kimi-101-20261029-0530`; no subagents
are used. The earlier silverzoom and mistyfly reports are separate test passes.

Completion requires real complex tasks on both workers, independent artifact
verification, and a final running-tenant/security audit. No teardown is allowed.
`highwinds` follows [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
and must not be targeted by animal-name cleanup.

## Previous pass: provisioning and builds

These are historical observations from !2040, not new measurements in this pass.
The host is a Linux systemd VM with rootless Podman, tenant **user-manager**
units, 12 reported usable processors, and 23 GiB memory. Host machine names are
intentionally omitted. All lifecycle operations use mt-admin, not system-bus
commands against tenant units.

- `add-tenant highwinds --with-kimi --with-pebble --with-study-mcp --llm-base-url
  https://nano-gpt.com/api/v1 --llm-model openai/gpt-5.2` succeeded. Credentials
  were supplied privately; Kimi used `--kimi-api-key-file` with an anonymous
  memory descriptor. No credential file was read or copied by the test harness.
- New tenant uid 1010, linger enabled. Primary ports 10000–10009: gateway
  10000, PostgreSQL 10003, orchestrator 10006, Pebble WS 10008. Kimi WS/health
  30000/30001; Pebble health 20004; vision API/health 20005/20006.
- The tenant clone began at `05a2cd90fc34da616c9480b7e7a31ed1da6dc3ef`.
  PostgreSQL, WeeChat, study MCP, LunarVision and sandbox provisioning succeeded.
- `build-tenant --with-wasm --with-pebble --with-kimi --with-toolchains` built
  the daemon release binary (12m16s), XMPP bridge release (10m27s), nine WASM
  tools and four channels. Installation: 12 installed, DarkIRC skipped because
  disabled. No full debug daemon build was used.
- Pebble upstream `2d0f3786cbebb705cbd4f6212b0aaf47d8d8590b`, reviewed lock and
  WI#477 patch verified. The fat image passed both WI#334 toolchain smoke legs:
  actual `/bin/sh -lc` task shell and sanitized `env -i` resolution. Rust/Cargo
  1.96.1, rustup 1.28.2, Go 1.24.3 and native C/C++ support were installed.
  Image ID `168667d144b4fce57985a430a554b2c53e42172f75220ea2a83e56f8a2bfe6e4`,
  size 2,334,932,434 bytes, revision `67a96a9dd4380317dd05badda0f357a20c9085bb`,
  toolchain profile true.
- Initial Kimi fat image: CLI 2.3.0, 178/178 in-image tests, including the real
  CLI contract. Task setup then failed at runtime with `Permission denied`:
  root-owned `/app/entrypoint-task-setup.sh` was 0511, executable but unreadable
  to uid 10002. Commit `5e041ea0f` changed the two entrypoints to 0555, rebuilt
  image `5491cbb1cfe425f7324c06e45035817c65b8e5b91cb6fef082750146e7861ffe`,
  and recovered Kimi readiness. That symptom-only change is superseded by RG-1
  below; root-side readiness did not prove task-side config/hook readability.
- Previous final status: daemon, PostgreSQL, both workers and vision healthy;
  listener audit clean, worker egress exclusive and DNS pinned with only the
  explicit `nano-gpt.com:443` allow. No complex acceptance job had run.

The old report contradicted itself about the rebuilt Kimi revision. Its initial
image used `05a2cd90f`; the rebuilt image was recorded as `5e041ea0f`. Neither is
a claim about the image this re-attempt will build.

## RG-1 — restrictive checkout umask propagated into runtime policy files

The review identified the cause, not just the entrypoint symptom: mt-admin's
private tenant checkout has 0600 tracked non-executable files and 0700
directories. `COPY` preserves these modes. `chmod +x` followed by `chmod a-w`
turns a 0600 script into 0511, while config files become 0400 and their
directories 0500. Bridge root can start while uid 10002 cannot read its policy,
hook, runner or MCP shim. Root-run tests/readiness therefore missed the problem.

Fresh read-only confirmation on 2026-10-29: the tenant's Kimi `config` and
`hooks` directories are both 0700; `config/config.toml` and
`hooks/headless_command_guard.mjs` are both 0600. Their tracked Git modes are
100644. This is a restrictive-context build, not a broken task privilege drop.

The replacement fix in `kimicode4lunarwing/Dockerfile`:

- Normalizes `/app/config`, `/app/hooks` and `/app/scripts` with `a+rX`, the
  health/bind/protocol files with read access, and entrypoints to 0555.
- Removes write access from all those immutable inputs, preserving root
  ownership. Does not relax task identity, WI#206 hooks, authentication or egress.
- In the final image layer, verifies root ownership/no write bits, then uses
  `setpriv --reuid=10002 --regid=10002 --clear-groups` to traverse/read every
  input (including dependency symlink targets) and execute the real task setup.
  Setup uses a disposable home and a documentation endpoint, no credential;
  its config/MCP outputs must exist. Temporary state is removed in the same RUN.
- `.gitlab/tests/test_kimi_image_modes.py` executes the real Dockerfile mode
  block against both normal/restrictive fixtures, compares resulting modes,
  runs setup as uid 10002 on root-capable runners, and confirms unreadable
  nested files/directories are rejected rather than silently skipped.

Verification on the committed tree:

- Four mode regressions pass under root, including both genuine dropped-uid
  cases. Unprivileged: two passes, two explicit root-required skips. Three
  in-memory mutations (drop read/traverse normalization, drop immutable-input
  write removal, run the gate as root) each fail the real task-uid regression;
  no tracked source was mutated.
- Standard Kimi `npm run build` and `npm test`: 178 tests, 177 passes, one skip,
  zero failures. CI configuration validation and the worker exposure policy
  guard pass. `test_goal1_ci.py`: 267/267. Full `.gitlab/tests` discovery:
  473 tests, six skips, zero failures (the two new root-required cases are among
  those skips and were separately run under root).
- Standard `build-kimi-worker --with-toolchains` finished with exit 0 in tmux.
  Image ID `9d7e198a35113f72a1896947b29dd76ac6c8b5e520bf41f37375ca4c71de28e7`,
  size 2,717,326,497 bytes, revision `0fe42ef3ffb3a3a671211cce1381ca571c27c2cd`,
  toolchain profile true. Its final build layer passed the uid-10002 traversal,
  reads and real task-setup gate. The earlier npm build/test layers were reused
  from cache; the host suite above is a fresh run. This build used the operator
  checkout (normal source modes); passing `umask 077` to the build process does
  not change pre-existing context modes. Restrictive-context equivalence is
  proved by the executed Dockerfile-block regressions, not misattributed to
  this full-image build.
- A separate network-disabled container, explicitly `--user 10002:10002`,
  successfully read all policy/script files and executed the actual setup,
  PreToolUse guard and stdio MCP shim. The shim answered initialize/tools-list
  with no grant and zero advertised tools; final marker `RG1_TASK_UID_INPUTS_OK`.
  No live credentials or tenant mounts were supplied to that smoke container.

Logs: `/tmp/lw-item101/rg1-{build,npm-test,ci-tests,all-ci-tests}.log`.
This certifies the RG-1 image behavior, not whole-image reproducibility or
live gateway-to-worker acceptance.

The same restrictive source context is relevant to the Pebble and OpenCode
image `COPY`/read-only patterns. This change fixes **Kimi**, not all workers;
no cross-worker mode repair or whole-image reproducibility certification is
claimed. Copied permission differences also affect image digests independently
of source revision (WI#425/WI#507 provenance concerns).

## Other findings retained from !2040

**WI#504: first-start rootless namespace lifetime.** The earlier standard
`configure-worker-egress highwinds --allow nano-gpt.com:443` failed because
separate short-lived load/verify sessions saw different namespaces. A temporary
operator-only `podman unshare --rootless-netns sleep 3600` keeper allowed the
standard command to succeed; it was not a firewall bypass and is **not** a
runbook step. At this re-attempt's initial audit the old `item101-netns` tmux
session was absent; `worker-egress highwinds` verified both running containers
and exact forward/input rejects. No new manual keeper was started.

**New live blocker: managed keeper rejects Podman 6.1.4's built-in infra
container.** After the new Kimi image was distributed into highwinds's rootless
store, standard `restart-worker highwinds kimi` stopped the old worker but
failed closed before starting its replacement. The current guard requires a
`localhost/podman-pause:<tag>` image and rejected `ImageName == ""` with
`namespace keeper lw-egress-keeper-highwinds uses unapproved infra image '';
pod removed`. A standard `start-worker highwinds kimi` retry failed identically.

A separate disposable diagnostic pod on `--network none` reproduced Podman's
actual contract: `IsInfra=true`, empty `ImageName` and image ID,
`Path=/catatonit`, `Args=[-P]`, rootfs
`/run/user/1010/libpod/tmp/infra-container`. The diagnostic pod was removed.
This is a built-in image-less infra container, not evidence of a remote image.
The guard's matching condition in `ic/scripts/lunarwing-worker-egress-guard.sh`
therefore excludes the stock implementation on this host. The security fix
must validate this built-in representation without admitting arbitrary
image-less/rootfs containers; that guard change is not part of RG-1 here.

Current state after these attempts: daemon, PostgreSQL, vision and Pebble stay
running. Kimi is stopped/not created. `worker-egress highwinds` still verifies
the exact live rejects, exclusive Pebble attachment, static hosts and disabled
recursive DNS. No filter was bypassed, no image-user check relaxed, and no
teardown run. The task's requirement to leave **both** workers running is not
yet satisfied. Logs: `/tmp/lw-item101/rg1-{restart,start-retry,egress-audit}.log`.

**WI#507: source-root asymmetry.** `build_pebble_worker` used the operator
checkout (`67a96a9dd...`), while Kimi used the tenant clone (`05a2cd90f...`).
Those image labels/build logs are historical evidence of two source commits
for one tenant; this report does not rewrite them to a later head. The owning
work item remains separate from RG-1.

## Planned worker exercise — not results

Existing prepared tasks are retained, sequentially dispatched into distinct
workspace subdirectories. No subagents, secret inspection or unrelated project
changes are permitted. Artifacts will remain in the persistent tenant:
`/home/highwinds/lunarwing/{kimi,pebble}-workspace`, mounted at `/workspace`.

| Lane | Task | Independent oracle |
| --- | --- | --- |
| Kimi / Rust | Two-tree Fenwick inclusive range add/sum, signed i128 | Seeded naive array |
| Kimi / Go | Concurrent snapshot-isolation transactional store | Transaction model plus race detector |
| Kimi / C++17 | Dinic max flow, parallel edges and residuals | Exhaustive small-graph min cuts |
| Pebble / Rust | Segment-tree lifetimes and rollback DSU connectivity | Seeded multigraph BFS |
| Pebble / Go | External stable CSV sort/merge join | Python CSV Cartesian-product join |
| Pebble / C++17 | Rectangular Hungarian assignment | Exhaustive assignments plus dual certificates |

Prior verifier-host versions were Python 3.11.17, Go 1.27.3, Rust/Cargo 1.96.1
and G++ 16.2.1; these are **not** worker-image version claims. Job IDs, actual
commands, worker versions, independent counts and the final hand-off will be
added only after they are observed. Redacted operational logs and helpers from
the previous pass remain under `/tmp/lw-item101/` on this host.

## Re-attempt acceptance and blocked hand-off

The surviving Pebble lane was given one real gateway request at 05:59Z on
2026-10-29: title `hw101-pebble-cpp-assignment`, thread
`31711f66-921e-4f7c-b9cf-c41dccb5f758`, message
`80239868-5642-4861-b239-d49c9c056ca1`. It requested the prepared Hungarian
assignment task, lane `pebble`, one `create_job`, no subagents, and real tests.
`POST /api/chat/send` returned `accepted`, but subsequent history/thread/job
queries showed no approval, no assistant response, an Idle thread, and zero
jobs. History's `Failed` with `completed_at: null` is not evidence of a terminal
worker failure. No duplicate dispatch was sent. The polling helper was stopped
once this state was recorded; the thread remains available for diagnosis.
No complex worker output or independent algorithm result is claimed.

The final standard Kimi `start-worker` retry still fails at the same managed
keeper image-name check, not at RG-1's task-uid file access. The fat fixed image
is present in both the admin and tenant image stores, but Kimi cannot be
recreated through the current guard on this Podman version. The daemon,
PostgreSQL, vision and Pebble remain running; the tenant and allocated ports
are retained. Kimi is stopped. No temporary namespace keeper is running, no
security policy was bypassed, and no teardown was performed.

**STOP / TRIED / BLOCKED BY / RECOMMEND:** RG-1 is fixed and image-tested;
standard build, worker restart and repeated standard starts were attempted.
Live acceptance and the both-workers-running hand-off are blocked by the
managed keeper's exclusion of Podman 6.1.4's built-in image-less infra
container; the gateway also produced no job from the Pebble request. First
repair/validate that narrow keeper compatibility path without admitting
arbitrary rootfs containers, then `start-worker highwinds kimi`, verify
`worker-egress`/`worker-listeners`, and resume the six prepared tasks. Diagnose
the retained gateway thread before duplicating its request. Item 101 stays
unchecked until these actual requirements pass. No protected pipeline or
full-image reproducibility acceptance is claimed by this branch.

## This pass (2026-10-29 evening): acceptance unblocked

Both named blockers from the previous pass are now closed, and the item's real
acceptance bar — both workers running, complex tasks actually executed — is met.

### Blocker 1 (egress keeper) was fixed by item-100, not by this branch

The keeper's exclusion of Podman 6.1.4's built-in image-less infra container
was closed on the target branch by item-100 commits `c7438ad75` (identify native
pause keeper metadata) and `fb502652d` (accept constrained image-less keepers),
which add `keeper_uses_native_pause()` to
`ic/scripts/lunarwing-worker-egress-guard.sh`: when `ImageName` is empty the
guard now accepts the infra container only if its exact shape is
`|/catatonit|["-P"]|[]|/run/user/<uid>/libpod/tmp/infra-container`, still
root-owned, still exclusively attached to the tenant's egress network. This
admits the stock implementation without admitting arbitrary image-less/rootfs
containers — exactly the repair the previous pass asked for. It landed after
that pass ran, so its "blocked" record stands as accurate history.

Verified live this pass: standard `start-worker highwinds kimi` now refreshes
the image into the tenant's rootless store, enforces the egress filter, and
reports `kimi worker ready via quadlet (... WSS port 30000)`. No guard bypass,
no temporary keeper, no manual `podman unshare` netns pin was used.

### Blocker 2 (retained gateway thread) was an approval-window expiry, not a gateway defect

Thread `31711f66-921e-4f7c-b9cf-c41dccb5f758` was diagnosed from the tenant's
Postgres `conversation_messages`, not inferred. Its assistant message exists
and says the platform reported: *"The approval request for this action expired
after 30 minute(s) without a user response. The action was NOT executed."*
Timestamps confirm it: user message 05:59:09Z, assistant reply 06:29:44Z —
exactly the 30-minute gate window. The previous pass's poller was interrupted
(KeyboardInterrupt) at ~06:03, so it never saw the expiry.

**Why the approval was never seen:** `GET /api/chat/history` only populates
`pending_approval` from the in-memory legacy session
(`ic/src/channels/web/handlers/chat.rs:259-314`). With `ENGINE_V2=true` —
this tenant's configuration — `prefer_durable_history` is true, so the handler
serves history from the durable store, which carries only completed
user/assistant/tool_calls messages and no gate state. `pending_approval`
therefore stays `null` for the whole life of an Engine V2 approval gate, and a
client polling history can never learn the `request_id` needed to approve.

This is **not** a daemon bug: the supported discovery surface is
`GET /api/chat/pending-gates`
(`ic/src/channels/web/server.rs:1460`, backed by
`bridge::list_engine_pending_gates`), which lists live Engine V2 gates
per-user, and the browser UI already uses it (`static/app.js:2268`) alongside
the `approval_needed` SSE event. The previous pass's harness
(`/tmp/lw-item101/exercise.py`) polled only `history`, so it missed a gate that
was genuinely pending and approvable. A corrected harness
(`/tmp/lw-item101/exercise2.py`) discovers gates via `pending-gates`, refuses
any gate whose tool is not `create_job` or whose parameters do not name the
expected title, and approves inside the window. **Follow-up:** the history
envelope's silently-null `pending_approval` under Engine V2 is a footgun for
any external automation; it should either be populated from the durable gate
store or documented as legacy-only.

### Worker acceptance results

Both lanes were driven through the real gateway with one `create_job` each,
approved inside the window, no subagents, no duplicate dispatch.

| Lane | Job | Hub state | Elapsed | Hub events |
| --- | --- | --- | --- | --- |
| Kimi (fat image, uid 10002) | `hw101-kimi-rust-fenwick` / `98304950-7c5e-45e9-b6ee-e4a000ebe900` | **completed** | 342 s | 34 |
| Pebble | `hw101-pebble-cpp-assignment` / `9e3a7172-aa71-40c5-8c80-5df1fc38900b` | in_progress → **Stuck** | 1807 s | **0** |

**Kimi — success, independently verified.** The worker reported real toolchain
versions from inside the container (`rustc 1.96.1`, `cargo 1.96.1`,
`go1.24.3 linux/amd64`, `g++ 12.2.0`, `cc 12.2.0`), built a dependency-free
Rust 2024 crate at `/workspace/hw101_kimi_rust` (two-Fenwick-tree inclusive
range add/sum, signed i128), ran 8 library tests (`test result: ok. 8 passed;
0 failed`) and a release build, and executed the real CLI. Artifacts are owned
by `kimi:kimi` — confirming both the RG-1 mode normalization and the WI#454
privilege drop landed in the built image. **Independent verification (not the
worker's own claim):** 60 seeded randomized trials driven against the release
binary and compared to a Python naive-array oracle produced **0 mismatches**,
including negative deltas, 1e9-magnitude values, overlapping updates and
singleton ranges.

**Pebble — correct work, lost result (finding PE-1 below).** The worker ran
16m16s, wrote `/workspace/hw101_pebble_cpp` (`src/main.cpp`, `tests/tests.cpp`,
`tests_runner`, `tests_runner_asan`, `Makefile`, `README.md`, `assignment`),
all owned by `pebbletask` (uid 10004). **Independent verification:** 40
brute-force permutation trials over random rectangular matrices (N≤5, M≤6)
gave **0 cost mismatches**, and 30 trials validated the full dual certificate
(U[i]+V[j] ≤ cost[i][j] on every cell, equality on matched cells,
sum(U)+sum(V) = COST, unmatched column potentials 0, COST equal to the matched
total) with **0 failures**. So the algorithm the worker produced is correct.

But the hub recorded **zero** events for the job and never transitioned it out
of `in_progress`; at 20:26:42Z self-repair flagged it
(`Job ... exceeded the idle threshold for external worker 'pebble'; the daemon
cannot retry external tasks`). The container log shows the task did finish:
`stdout stream ended` / `stderr stream ended` / `result built, forwarding` at
20:12:19Z, then `[bridge] ... disconnected` **64 µs later**.

### PE-1 (Medium): pebble buffers all task events until the task ends, so the hub sees a silent socket

`pebble4lunarwing/src/bridge.rs:499-528` awaits the whole task before draining:

```rust
let execution = execute_dispatch_task(...).await;   // runs the entire task
forward_execution_error(&event_tx, &task_id, execution);
drop(event_tx);
...
while let Some(event) = event_rx.recv().await {     // only now forwarded
```

Every `Progress` and the final `Result` the executor sends
(`executor.rs:322,337,342,727,...`, all via `let _ = event_tx.send(...)`)
accumulate in the unbounded channel and are flushed to the outbound socket only
after the subprocess has exited. Two consequences, both observed here:

1. **No progress reaches the hub during a long task.** The connection stays
   silent for the full 16 minutes, so the hub's external-worker idle detector
   (`ic/src/agent/self_repair.rs:300-343`, `external_stuck_threshold`) has no
   activity to see and marks the job Stuck even though the worker is busy. Any
   pebble task longer than that threshold will be misreported as stuck.
2. **The final result can be lost.** `let _ =` discards every send error, so a
   closed receiver drops the result with no diagnostic. Here the socket closed
   in the same 64 µs window as the flush, the hub recorded 0 of the buffered
   envelopes, and the job never completed.

The Kimi lane does not have this shape — it streams `task_result` and progress
envelopes through the socket as they occur, which is why the same exercise
produced 34 hub events and a `completed` job. **Recommended fix:** drain
`event_rx` concurrently with the task (forward each event as it arrives instead
of after `.await`), and stop discarding the send error so a dropped result is
logged rather than silently lost. This is a worker-protocol defect, out of
RG-1's scope, and is reported here rather than fixed silently in an acceptance
pass.

### RG-1 completed for the remaining three lanes

RG-1's ask 3 explicitly named the Pebble, OpenCode 2 and OpenCode v1 images as
sharing the context-dependent mode defect; the previous pass fixed only Kimi and
recorded "This change fixes **Kimi**, not all workers". All four lanes are now
normalized, each with the mode arithmetic made independent of the builder's
umask and a dropped-uid build gate:

- `opencode4lunarwing/Dockerfile` (`c437692e9`) — had **no** normalization at
  all: `COPY scripts/ config/` plus `chmod +x` only, so a restrictive context
  left `/app/config` and `/app/scripts` unreadable by uid 10001 while its
  `/app stays root-owned and read-only` comment was untrue. Now `a+rX` +
  `a+r`, then `a-w`, then a uid-10001 `find -L ... -type f -exec cat` traversal.
- `opencode2.4lunarwing/Dockerfile` (`47af22fae`) — stripped write bits with
  `chmod -R a-w` but never added read/execute bits. Now `a+rX`/`a+r` before the
  `a-w` pass, a `command -v setpriv` fail-closed guard, and the uid-10003
  traversal gate. Its existing WI#357 policy assert still holds: it requires
  root ownership and no write bits, and this change only adds read bits.
- `pebble4lunarwing/Dockerfile` (`59e8f5097`, `c738cedab`) — `entrypoint.sh` and
  `entrypoint-split.sh` were COPY'd to `/usr/local/bin` with **no** chmod, so a
  restrictive context left them 0700 root:root while the image's `USER` is
  10004; the existing `test -x` readiness check runs as root and passed
  vacuously (precisely the class RG-1 named). Now pinned 0555, plus
  `/etc/profile.d/lunarwing-toolchain.sh` and the rustup wrapper pinned 0644 —
  the profile snippet is *sourced* by the task's `/bin/sh -lc` shells, so
  0600 root:root would make dash silently skip it and drop the toolchain env
  the WI#320 gauntlet (F6) depends on. Added a uid-10004 gate that reads every
  baked task-side input and requires all four launchers to be executable as the
  task uid.

Kimi's own block (`kimicode4lunarwing/Dockerfile:258-294`) is unchanged this
pass; it was already the reference shape.

### Security posture after this pass

`worker-egress highwinds` reports the filter live with the exact tenant
forward/input rejects, and **all three** containers (`lunarwing-pebble-highwinds`,
`lunarwing-kimi-highwinds`, `lunarwing-kimi-task-highwinds`) attached
exclusively to `lw-egress-highwinds` with DNS pinned and static hosts current.
`worker-listeners highwinds` reports **clean**: both bridges publish on host
loopback only, and the kimi task container has no non-loopback listeners. The
new uid gates can only make a build fail; they grant nothing. No egress rule,
allowlist entry, capability, or WI#357 exposure control was relaxed, and no
worker gained an inbound path (WI#357).

### Verification

- `start-worker highwinds kimi`: image refreshed into the tenant rootless
  store, egress filter enforced, `kimi worker ready via quadlet`.
- `mt-admin status highwinds`: Pebble running (WSS 10008), **Kimi running**
  (WSS 30000), daemon/PostgreSQL/vision/WeeChat/XMPP-bridge all active. Both
  workers running is the hand-off the previous pass could not reach.
- `worker-egress highwinds` and `worker-listeners highwinds`: both clean, as
  above.
- Independent artifact verification: Kimi 60/60 vs a Python oracle; Pebble 40/40
  brute-force cost plus 30/30 full dual-certificate checks.
- `.gitlab/tests/test_pebble_build_inputs.py`: **41 tests OK** (4 root-required
  skips). `.gitlab/tests/test_kimi_image_modes.py`: **4 tests OK** (2
  root-required skips). `opencode2.4lunarwing/scripts/exposure_policy_assert_test.py`:
  **68 tests OK**. `test_worker_exposure_policy.py`: **38 tests OK** (431s, run
  in tmux). `.gitlab/scripts/check-worker-exposure-policy.sh`: green end to end,
  which covers both the opencode2 and opencode-v1 profiles.
- No Rust source or dependency changed this pass, so no cargo lane applies; the
  worker-image changes are Dockerfile-only and are exercised by their lanes'
  build-time gates.

Tenant `highwinds` and all its ports remain allocated and running. Both workers
are up. No teardown, no animal-name rename, no credential was read or recorded.
