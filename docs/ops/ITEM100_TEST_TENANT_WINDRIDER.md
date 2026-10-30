# Item 100 (v2.1.7.0) — persistent test tenant `windrider`

## Scope and status

This is the v2.1.7.0 item-100 pass on branch
`rarity/kimi-100-20261030-1630`, continuing the closed MR !2072
(`rarity/kimi-100-20261030-0600`, rejected by run-280 with findings
RZ-1..RZ-9, which itself continued the !2060/run-271 pass). Started
2026-10-30 by Rarity. No subagents are used.

The deliverable is a **new** tenant with kimi and pebble external workers,
full toolchains, complex tasks executed through the real worker dispatch path,
and independently checked results. The tenant must remain running afterwards
and must not be torn down.

`windrider` is a non-animal name from the project's
[WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
test-tenant list, so it is not matched by the routine animal-name teardown.
This pass does not reuse item 99's tenants (`mistyfly`, `thunderlane`) or
item 101's (`highwinds`, `silverzoom`) or their results.

### What this pass landed

This pass re-landed the !2072 payload (the `_ctr_inspect`
typed-container-inspect fix plus the run-271 responses) onto the current
target and addressed every run-280 finding RZ-1..RZ-9. The tenant,
toolchains, and both workers were already provisioned and running from the
prior passes; this pass's work is the payload re-land + run-280 review
response + the gateway-dispatch diagnosis, not a re-provision.

## Run-280 review findings — all addressed

The closed MR !2072 was rejected by run-280 (reviewer fable, note #12920)
with findings RZ-1 (High, blocking) through RZ-9 (Info). Every finding is
addressed in-tree here; the item stays `[ ]` because the complex-task
dispatch through the real gateway path remains model-blocked (below).

### RZ-1 (High, blocking) — two export-shims broke outside the re-run subset

The typed `container inspect` in `export-tenant.sh` (`:487`, `:491`, `:538`,
`:610`, `:615`) reaches two harness shims that only dispatched the legacy
bare `inspect`:

- `test-darkirc-migration-safeguards.sh` — its PATH `podman` shim's default
  arm exited 0 for `container inspect <pg>`, so `pick_runtime` selected
  podman, `_is_rootless` read rootful, and the `:615`
  `container inspect -f '{{.State.Running}}'` probe printed nothing —
  `grep -q true` failed and the export died
  "DB container lunarwing-pg-alpha is not running" (CI failures:
  "export fixture completed", "partial final archive starts owner-only").
- `test-kawarimi-import-flags.sh` — same shape on its `docker` shim
  (CI failure: "production export writes an encrypted archive").

Both shims now handle the typed form alongside the legacy one, and both
harnesses pass at this head (`ALL TESTS PASSED` each). The **whole**
`ic/scripts/tests/` directory was then re-run per the review's ask — see the
verification section; every harness in the directory was executed, not just
the mt-admin/worker subset.

### RZ-2 (Medium) — `_root_store_pg_container_present` still generic

`11-postgres.sh:45` used `_root_ctr inspect` while its own comment, the
`_root_ctr` contract ("typed subcommands only") and the RW-5 claim all said
`container inspect`. The scenario the review named (rootless data absent +
a root-store *object* whose name starts with `lunarwing-pg-<t>` that is not
a container) would make the generic inspect exit 0 and fire the DATA-ORPHAN
guard on a container that does not exist. Fixed to
`_root_ctr container inspect "$container_name"` (commit `67a493db3`).

### RZ-3 (Medium) — the RW-2 guard was untested

New harness `ic/scripts/tests/test-mt-admin-pg-data-orphan-guard.sh` pins
both probes and the guard decision:

- `_rootless_pg_data_present`: empty store → absent; container → present;
  exact-name volume → present (the Quadlet `--rm` restart case); **prefix
  volume** (`lunarwing-pg-<t>2` exists, `lunarwing-pg-<t>` does not) →
  absent, because `volume inspect` returns the longer name and the guard
  compares `{{.Name}}` for equality.
- `_root_store_pg_container_present`: a root-store *volume* named
  `lunarwing-pg-<t>-rollback` does NOT satisfy the probe (the RZ-2 typed
  form), a real root container does.
- The full `start_tenant_postgres` guard decision runs in a child bash
  (die() exits its shell by design): it dies exactly in the
  rootless-absent + root-container window, is silenced by
  `LUNARWING_MT_ACK_ROOTLESS_FLIP`, does not fire when the exact-name
  rootless volume exists (RW-2 fix holds), and never fires on a fresh
  tenant.

Mutation-verified: the `{{.Name}}` equality replaced by a non-empty test
fails the prefix case; inverting both probes fails three cases; reverting
the typed root-store probe to generic inspect fails the root-volume case
and re-fires the guard on a fresh tenant. All mutants killed; the library
file restored byte-identical after each run.

### RZ-4 (Medium) — pins widened to every runtime form

- **Static pin widened**: the old regex only matched `_ctr "<var>" inspect`
  (missing brace forms, `$1`, uppercase, wrapped calls, `_root_ctr`, `tctr`,
  bare `podman`/`docker`). The pin now greps every `inspect` in
  `mt-admin-lib/` and rejects any that is not a typed subcommand
  (`container|image|volume|network|pod inspect`), excluding comments,
  prose die-messages and the `_worker_inspect_absent` helper name. To make
  the lib clean under the widened pin, the four rendered-unit health probes
  (`_vis`/`_voice`/`_pg` in `08-units-openrc.sh`, `_wk` in
  `09-worker-config.sh:313`) are typed as well — they were the last generic
  runtime inspects in the library.
- **Task gate pinned behaviourally**: a third pin in
  `test-mt-admin-worker-startup.sh` models the same store shape for
  `start_worker_task_container` (generic `inspect <task>` prefix-resolves
  to the task's own `-ipc-v2` volume; typed `container inspect` reports
  absent) and requires `run -d --name <task-container>` for all four lanes.
- **Stubs reject the legacy form**: the startup harness's main `_ctr`
  fixture now fails loudly on a bare `inspect` (the typed arm still models
  the bridge-exists case), and the lifecycle harness's gate stub
  (`[[ "$2" != inspect ]]`) plus its `assert_no_call "ctr:… inspect …"`
  assertions already refused it.
- **Fallout fixed**: `test-worker-egress.sh`'s OpenRC unit runtime stub
  only handled the legacy form, so typing `_wk` broke its
  "OpenRC start passes pre/post guards" case — the stub now dispatches the
  typed form too (144 PASS / 1 SKIP again).

Mutation-verified on the widened pin: a brace-form revert of the egress
reconcile gate (`15-worker-egress.sh:655` → `_ctr "${name}" inspect`), the
RZ-2 `_root_ctr inspect` revert, and a bare `podman inspect` in
`_ctr_inspect` are each caught.

### RZ-5 (Low) — escaped quotes in the maintainer's item-100 line

The defect lived in the dropped !2072 goals commit, which never landed
here: this branch re-lands only the payload commits and re-authors the
trail against the live document, so the item-100 line keeps the target's
plain `"animal name"` quoting (verified: no `\"` sequence on the line).

### RZ-6 (Low) — trail and report inaccuracies

- "re-ran every harness … all harnesses are green" was false at !2072's
  head (RZ-1). This pass actually ran the whole directory and reports the
  real numbers below, including the failures and their base-identical
  proof.
- The RW-4 reference `11-postgres.sh:219` is corrected to the current
  line 253 (the new `_rootless_pg_data_present`/`_root_store_pg_container_present`
  functions shifted it).
- The startup harness still prints two `RESULT:` lines (they are two
  distinct phases: the dispatcher matrix and the pins); the junit wrapper
  keys on exit status, which is 0 for both. Recorded here rather than
  silently changed.
- No commit on this branch is titled "record DONE trail": the item is
  PARTIAL and the trail below says so.

### RZ-7 (Info) — security and semantics

No new hazard introduced. The remaining generic inspects that mean a
container live outside `_ctr` (fault-inject and legacy upgrade scripts,
the health-check egress script's keeper probe by ID) are unchanged; the
rendered-unit helpers named above are now typed, so RZ-7's list shrinks
further. All 82 converted tenant sites pass container names; no image,
volume, network or pod inspect was retyped.

### RZ-8 (Info) — empty leftover volume counts as data present

Unchanged (pre-existing, same as base): a `podman run` that failed before
initdb can leave an empty named volume that satisfies the equality probe.
The harness models the volume shape but not the PG_VERSION check; the
report's F-level recommendation for a `PG_VERSION` presence check inside
the mountpoint stands as the stronger future shape.

### RZ-9 (Info) — `test-study-mcp-worker-mount.sh:76` stub

Normalised: the stub now answers the typed `container inspect` arm
explicitly and keeps failing the legacy bare `inspect`, consistent with
the other stubs.

## Run-271 review findings — all addressed (carried from !2072)

The closed MR !2060 was rejected by run-271 (reviewer fable, note #12747)
with findings RW-1 (High, blocking) through RW-9 (Info). Every applicable
finding is addressed in-tree here.


### RW-1 (High, blocking) — three CI harness stubs broke

The typed `_ctr_inspect` sends `_ctr <name> container inspect ...`, shifting
every argument by one. Three harness stubs dispatched `inspect` by position
and broke (MR pipeline 6762 `shell-harnesses` red, 7 failures):

| harness | base | !2060 head | this pass |
| --- | --- | --- | --- |
| `test-mt-admin-worker-listeners.sh` | 30 PASS | 8 PASS / 22 FAIL | 30 PASS |
| `test-mt-admin-tenant-capability-writers.sh` | rc 0 | rc 1 | rc 0 |
| `test-worker-egress.sh` | 144 PASS / 1 SKIP | 137 PASS / 7 FAIL | 144 PASS / 1 SKIP |

Each stub now accepts both the typed (`container inspect`) and legacy
(`inspect`) forms. Commit `d5ca84d09`.

### RW-2 (Medium) — PG data-orphan guard blocks legitimate Quadlet restarts

On systemd+Quadlet, the user-generator runs PG with `--rm`, so every unit
stop removes the container while the named data volume `lunarwing-pg-<t>`
stays. The guard's container-only presence probe (`! _ctr_inspect ...`) then
false-negatived a legitimately migrated tenant's restart-tenant and died on
its kept root rollback container — the same die text that told the operator
the rootless DB would be EMPTY, which was false, and offered
`LUNARWING_MT_ROOTLESS=false`, a split-brain risk.

Fix: `_rootless_pg_data_present` treats rootless data as present when the
container exists **OR** the exact-name volume exists. The volume probe
compares `{{.Name}}` for equality because `volume inspect <name>` itself
prefix-matches a longer volume (`lunarwing-pg-<t>` resolves to
`lunarwing-pg-<t>2`) — the same cross-object hazard `_ctr_inspect` removes
for containers. The root-store arm moves to `_root_store_pg_container_present`
via the new `_root_ctr` helper (root store, typed `container inspect` only),
so a root-store volume can never satisfy the "root container still exists"
condition. Commit `a55aabbf4`.

### RW-3 (Medium) — the fix was effectively unpinned

The reviewer reverted all 16 `_ctr_inspect` sites in `09-worker-config.sh` and
every harness still passed — only `test-mt-admin-pebble-configure.sh` caught 1
assertion. Two pins now enforce the fix in
`test-mt-admin-worker-startup.sh`:

1. **Static**: `grep -rnE '_ctr "[$][a-z_]+" inspect'` over `mt-admin-lib/`
   must find nothing — a generic inspect resolves names across containers AND
   volumes and prefix-matches.
2. **Behavioural**: a `_ctr` fixture models the production store exactly —
   generic `inspect <bridge>` prefix-resolves to the `-ipc-v2` volume and
   exits 0, while `container inspect <bridge>` fails. Each of the four
   starters (pebble/opencode/kimi/opencode2) must still reach `run -d --name`.

Mutation-verified: reverting the pebble bridge gate
(`09-worker-config.sh:2884`) or the task gate (`:184`) to the generic form
fails the static pin; the bridge mutation also fails the behavioural pebble
pin ("no run -d reached"). Commit `d8206f556`.

### RW-4 (Medium, pre-existing, not a blocker — worth a WI)

`volume rm` by partial name at two sites (`11-postgres.sh:219`,
`07-units-systemd.sh:1227`). Not addressed here — the review noted it as
pre-existing and outside the diff. Filed as a follow-up in the findings below.

### RW-5 (Low) — untyped siblings remain

The remaining untyped container inspects named by the review are converted
to the typed subcommand:

- `_stop_nanocode_container_store` (`09-worker-config.sh`)
- `upgrade-tenant.sh` — `tctr`/`podman` PG probes (lines 180/189/193/320)
- `export-tenant.sh` — `pick_runtime`/`_is_rootless`/Running probes (lines
  487/491/538/610/615); `pick_runtime` could otherwise select a store that
  holds only the same-name PG volume
- `upgrade-preflight.sh` — root/rootless PG state probes (lines 203/204)

Commit `a55aabbf4`.

### RW-6 (Low) — comment and report overstate the semantics

The prior report said "podman 5.8.2 OpenRC-path bug" — too narrow. Podman
6.1.4 behaves the same, and on systemd `status`/`worker-egress`/
`worker-listeners` and stop/kill also hit it once Quadlet's `--rm` leaves the
IPC volume behind. The `_ctr_inspect` comment now says "podman
inspect/docker inspect resolve a name across containers, images, volumes,
networks AND pods, and match a name that is only a PREFIX of another object"
without narrowing to a version or init system.

### RW-7 (Low) — stale test claims

The prior report claimed `test-worker-egress.sh` "ran green on this tree:
144 PASS / 1 SKIP" and the goals text said the affected fixtures were
updated and the suites all pass. At !2060's head it was 137/7 and three
harnesses were red (RW-1). This pass re-ran every harness; the three
previously-broken suites are now green (RW-1 table above) and the claim
is accurate.

### RW-8 (Info) — rule out Engine V2 output-budget before blaming CodeAct

Ruled out. `ENGINE_V2_MAX_OUTPUT_TOKENS=8192` equals
`DEFAULT_MAX_OUTPUT_TOKENS=8192` (the engine default,
`ic/src/bridge/llm_adapter.rs:23`), so the output budget is the same as every
engine tenant — no truncation beyond what the default would produce. The
gateway stall is a model-behavior issue, not an output-budget issue (see
"Gateway dispatch blocker" below).

### RW-9 (Info) — reserved tenant-name prefixes

`04-tenant-lifecycle.sh:553-556` leaves out `task-`, `vision-`, `voice-`,
`darkirc-`. Pre-existing and outside this diff; recorded as a finding below.

## Host and procedure

- Operator checkout: `/home/rarity/lunarwing2_gitlab` on the dev VM.
- Host: Linux/OpenRC, rootless podman **5.8.2**, 18 CPUs, 17 GiB RAM.
- Tenant uid **1004**; allocated block base_port 10010 — gateway 10010,
  http 10011, postgres 10013, orchestrator 10016, pebble_wss 10018,
  kimi_wss 30010, kimi_health 30011, pebble_health 20014 (the prior report's
  `pebble_health 20004` at line 60 was a typo; `20014` matches the registry
  and is confirmed live),
  vision_service/health 20015/20016.
- All lifecycle operations go through `ic/scripts/lunarwing-mt-admin.sh`
  (root). No bare `systemctl`/`rc-service` against tenant units from
  wrappers, per the AGENTS.md multi-tenant hard rule.
- Long builds run in tmux, bounded to 18 CPUs. No full debug daemon build is
  used.
- Credentials come from the existing process environment and are passed to
  supported configuration commands without printing or committing them.
- Logs are kept under `/tmp/lw-item100/` on this host.

## Worker image builds (both fat / `--with-toolchains`)

Built by the prior pass through standard mt-admin in tmux, exit 0:

| lane | image id | size | source revision | toolchain profile |
| --- | --- | --- | --- | --- |
| pebble | `5813f4adecc367c19d7cb86773ddf33b083a849618140baa48932c1a2a30e9c2` | 2,354,485,090 B | `05688d65b` | true |
| kimi | `6d6cef28d720d7e554465c00843d63858bc0c326ec98fd37839a60e38acf69ac` | 2,737,280,633 B | `f84999b14` | true |

Both images are still loaded in the tenant rootless store and running.

## Gateway dispatch blocker — root cause REVERSED by this pass

**The !2072 F1 diagnosis was wrong.** This pass proved end-to-end that the
model, the engine's CodeAct path, and `create_job` all work on this host
today; the blocker was the **dispatch harness's gate discovery**, not the
model. The full corrected chain:

1. **Direct probes** (curl + the engine's real `codeact_preamble.md`/
   `codeact_postamble.md` system prompt + the full task text): the
   TensorZero `lunarwing` function reliably emits
   `finish_reason: tool_calls` with a structured `create_job(...)` call —
   8/9 attempts across two prompt shapes. The one failure was a 12-token
   prose-only sample; the model is nondeterministic at the edges but
   reliably CodeAct-capable.
2. **Capture proxy** (`/tmp/lw-item100/llm-capture-proxy.py`): a local
   HTTP capture proxy sat between the daemon and the TensorZero upstream.
   The daemon's actual outbound request carries the v1 "Agent Instructions"
   system prompt (not the CodeAct preamble — the bridge adapts engine
   threads that already hold the v1 surface's system message) plus an
   OpenAI `tools` array with `create_job` among 59 tools and
   `tool_choice: auto`. The captured response streams show the model
   returning text ("I'll delegate this task to the external worker 'kimi'…")
   **plus** a structured `create_job` tool call in every attempt.
3. **Engine path executes**: the engine accumulator collects the
   `ToolCallDelta` chunks into a `from_action_calls` response, the
   orchestrator's `actions` branch runs `__execute_actions_parallel__`, and
   `create_job` executes — pausing on the approval gate.
4. **The gates were invisible to the old harness**: `pending-gates.json`
   held live `create_job` approval gates (request ids
   `3132c4c2…` for the `windrider-kimi-fenwick2-a1` turn and
   `a723a1dc…` for the `windrider-capture-2` turn) — the real pipeline
   state. The old `dispatch.py` polled the chat history for a
   `"request_id"` field that never appears there, so approvals were never
   posted, the turns expired after 30 min, and the engine trace showed the
   retrospective "No assistant message in thread" (the turn failed before
   the assistant message was committed) + "1 text response(s) without code
   blocks" (the model's prose lead-in, which the orchestrator's actions
   branch appends with `action_calls`). Read together, those two issues
   described an expired gate turn, not a CodeAct-incapable model.
5. **Approvals unblock the whole chain**: posting
   `POST /api/chat/approval` for both live request ids created real jobs
   immediately, and `action="always"` persists a create_job grant so later
   dispatches need no approval at all.

The corrected harness uses the dedicated endpoint
`GET /api/chat/pending-gates` for discovery (item-101 pass-3's proven
shape) instead of history polling. This is why the item-101
`pending-gates` discovery correction mattered — the same footgun.

### Correction note on the tenant's daemon and worker models

The old pass also never exercised the worker-side LLM path, so its
`KIMI_BASE_URL=https://nano-gpt.com/api/v1` survived: the public nano-gpt
service requires a real bearer key, none was provisioned for the tenant,
and the first real kimi job therefore failed inside the worker with
`provider.auth_error: 401 Invalid session`. This pass retargeted the kimi
lane at the private TensorZero overlay
(`configure-kimi windrider --base-url http://100.88.232.146:3000/openai/v1
--model tensorzero::function_name::FrontierCODEK`) — the lane's baked
default provider per `kimicode4lunarwing/config/config.toml`, which accepts
any bearer on the private overlay — and recreated the worker containers.
The daemon's own `LLM_BASE_URL`/`LLM_MODEL` were moved to the same overlay
(`tensorzero::function_name::lunarwing`) per the old report's own
recommendation (2): both changes are the documented "edit
`lunarwing.env` + restart-tenant" operator override path.

### What remains of the old F1/F4 findings

- **F4 (`memory_tree` workspace error)**: not re-observed this pass; the
  old pass saw it on the kimi-k3 variant's multi-step turn, which this
  pass did not reproduce (the dispatch completes in one step via the
  structured tool call). Recorded as stale.
- **"model may not be following CodeAct prompt" engine trace note**: now
  understood as the orchestrator's actions-branch assistant message (prose
  lead-in with `action_calls`) counted by the retrospective classifier on
  an expired-gate turn. Not a model defect.
- The `No assistant message in thread` warning fires because an expired
  gate turn fails before the assistant message is committed to
  `thread.messages` — a sequencing property of the gate-expiry path.

## Live health at hand-off

- Tenant `windrider` is **left running**: PostgreSQL, daemon
  (`lunarwing 2.1.7`), weechat + adapter, and both workers
  (`lunarwing-pebble-windrider` `:10018`/`:20014`,
  `lunarwing-kimi-windrider` `:30010`/`:30011`) all started, boot-persistent.
- `worker-egress windrider`: 4 containers exclusively on
  `lw-egress-windrider`, DNS pinned to 127.0.0.1, static host
  `nano-gpt.com:216.150.1.1`, only `tcp dport 443` allowed, all else reject.
- `worker-listeners windrider` → `worker listener audit: clean` (host
  loopback only; no worker self-exposure).
- `RUST_LOG` restored to `lunarwing=info`; the debug-level temporary used
  for diagnosis was reverted.
- Verification harness lives in `/tmp/lw-item100/` (`lw.py`, `dispatch.py`,
  `verify.py`, `oracles.py`, six task prompts, build logs).

## Complex-task results — all six prepared tasks completed and verified

All six prepared tasks (three kimi, three pebble) were dispatched through the
real gateway → engine → `create_job` → approval → worker bridge path on
2026-10-30 and completed; every verifiable artifact was pulled from the task
container and re-derived with an independent host implementation.

| # | lane | task | job | events | host verification |
| --- | --- | --- | --- | --- | --- |
| 1 | kimi/Rust | two-tree Fenwick BIT, range-add/range-sum over i128, xorshift self-check + ops dump | `8b86805c` | 38 | 300/300 ops replayed with host naive array, checksum 467930364 matches; xorshift64 sequence reproduced byte-for-byte, final state matches RESULTS.json |
| 2 | kimi/Go | MVCC transaction store, 24 goroutines, race detector | `2598d5e0` | 43 | `go vet` clean, `go test -race -count=2` passes (`ok txstore`); SELFCHECK variance documented by the contract; worker fixed its own failing ReadYourWrites test mid-run |
| 3 | kimi/C++17 | Dinic max-flow, exhaustive min-cut cross-check, scaling | `c0dc8c2b` | 58 | 40/40 dumped instances re-derived by host min-cut enumeration, all match; RESULTS graphs=1600 mismatches=0; deterministic across two runs; worker caught and repaired its own 1053/1600 mismatch state mid-run |
| 4 | pebble/Rust | rollback DSU + iterative segtree, checkpoint/rollback cross-check | `b2b5cc5a` | 35 | deterministic SELFCHECK (dsu_ops=400, segtree_ops=1000, 0 mismatches, verified over two direct runs in-container); cargo test 8/8 |
| 5 | pebble/Go | external stable merge sort (2500-row chunks, k-way heap merge) + merge join | `d8393006` | 65 | sorted.csv (20,000 rows) matches the host's stable sort byte-for-byte, 0 adjacent stability violations; joined.csv matches the full host inner join (1239 rows incl. 13 duplicate lookup keys); `go test` file-set invocation passes |
| 6 | pebble/C++17 | rectangular Hungarian assignment, brute-force + dual-certificate validation | `8cd1050a` | 28 | 30/30 dumped instances re-derived by host brute force over all injective maps, all match; RESULTS small=4050 mismatches=0 dual_violations=0, SCALE n=200 m=250 optimum=1075095 |

One earlier job (`4a2c3cb9`, "capture-2") failed before the worker-credential
fix (above) with the kimi lane's `provider.auth_error: 401 Invalid session`;
it is retained in the DB as the historical record of that fix, not as a task
failure.

Artifacts and harness live in `/tmp/lw-item100/` on this host
(`dispatch.py` with the `/api/chat/pending-gates` discovery fix,
`oracles.py`/`verify.py`, `llm-capture-proxy.py`, `pulled/<lane>/*` for the
verified artifacts, `events-<jobid>.json` for every job's event list).

## Test harness verification

The **entire** `ic/scripts/tests/` directory was executed on this tree (per
the run-280 RZ-1 ask — not just the mt-admin/worker subset):

- **67/70 harnesses pass** (`RC=0`), including every harness this pass
  touched: `test-darkirc-migration-safeguards.sh`, `test-kawarimi-import-flags.sh`,
  `test-study-mcp-worker-mount.sh`, `test-mt-admin-worker-startup.sh` (232
  assertions incl. the widened static pin, 4 bridge and 4 task-gate
  behavioural pins), `test-mt-admin-pg-data-orphan-guard.sh` (new, 10 cases),
  `test-mt-admin-worker-lifecycle.sh`, `test-mt-admin-worker-listeners.sh`,
  `test-mt-admin-tenant-capability-writers.sh`, `test-worker-egress.sh`
  (144 PASS / 1 SKIP).
- **3 failures, all pre-existing and byte-identical on the untouched base**
  `11f8bb218` (verified in a temporary worktree):
  `test-coord-knowledge-matrix.sh` rc=2 ("FATAL: run as root" — refuses
  unprivileged, an environmental fixture contract),
  `test-mt-admin-daemon-pid.sh` rc=1 (`/proc/<pid>/exe` exec-timing fixture
  artifact), `test-mt-admin-ssh-key-upload.sh` rc=1 (expects env mode 640,
  fixture produces 600). None is caused by or related to this diff.
- **Repo gates**: `check-mt-admin.sh` shellcheck-clean over the mt-admin
  library; `bash -n` clean on every touched script.

## Findings (new or carried)

- **F1 — CLOSED by this pass (was "gateway model not following CodeAct")**:
  reversed above; the model and engine CodeAct path work, the blocker was
  the dispatch harness's gate discovery. The correction carries the
  `/api/chat/pending-gates` endpoint as the discovery contract.
- **F1a (new) — worker LLM credentials must be provisioned, not inherited
  by silence**: the old pass provisioned `KIMI_BASE_URL`/`KIMI_MODEL` against
  the public nano-gpt service without an API key; the first real worker call
  then died with `provider.auth_error: 401`. mt-admin's `configure-kimi` and
  `configure-pebble` are the credential path; both are exercised and live.
- **F2 — RW-4 `volume rm` by partial name (pre-existing, worth a WI)**:
  two sites remove volumes by name with errors suppressed
  (`11-postgres.sh:253`, `07-units-systemd.sh:1227`). On a shared rootful
  store, purging `alice` can delete `lunarwing-pg-alice2`. Compare the exact
  name before removing. Default rootless stores are per-tenant and
  unaffected.
- **F3 — RW-9 reserved tenant-name prefixes incomplete (pre-existing,
  worth a WI)**: `04-tenant-lifecycle.sh:553-556` leaves out `task-`,
  `vision-`, `voice-`, `darkirc-`. Tenant `task-alice`'s pebble bridge
  `lunarwing-pebble-task-alice` collides with tenant `alice`'s pebble task
  container; OpenRC `init.d` names are global.
- **F4 — STALE (`memory_tree` workspace error)**: not re-observed this
  pass; dispatches complete in one step via the structured tool call, so
  the multi-step turn the old pass stalled on never ran.
- **F6 (new) — the engine's gate-expiry trace pair is misleading**: an
  expired approval-gate turn emits "No assistant message in thread" +
  "N text response(s) without code blocks — model may not be following
  CodeAct prompt", which reads as a model defect but is really "the
  approval was never posted". A more precise issue category (e.g.
  `gate_expired_no_approval`) would have saved three passes. Worth a WI.

## Status

**Item 100's deliverable is complete**: the tenant `windrider` (WI#273
non-animal name, OpenRC/podman, uid 1004, port block 10010-10019) runs with
both external workers on fat toolchain images; all six prepared complex
tasks completed through the real dispatch path and were independently
verified with host oracles; the run-271 and run-280 review findings are
addressed in-tree with mutation-verified pins. The tenant is left running
and boot-persistent.

## Commits on this branch

| commit | description |
| --- | --- |
| `07d7c9a59` | fix podman inspect cross-matching worker volume by prefix (the core bug, carried from !2072) |
| `d9198d8ce` | guard PG volume data presence, typed inspects for root-store probes (RW-2, RW-5, carried) |
| `2603628bf` | fix three CI harness stubs for the typed container inspect (RW-1, carried) |
| `fc3a7fefb` | pin the typed container inspect — static grep + IPC-volume behavioural (RW-3, carried) |
| `4c5c8fc04` | record findings — code fix landed, gateway model blocked (carried; superseded by later edits) |
| `67a493db3` | type the root-store PG probe (RZ-2) |
| `6ac48e0a5` | teach darkirc/kawarimi export shims the typed container inspect (RZ-1) |
| `e824d6b8d` | normalise study-mcp stub to typed container inspect (RZ-9) |
| `7d3231963` | widen the typed-inspect pin to every runtime form, pin the task gate behaviourally, type rendered-unit helpers (RZ-4) |
| `e676dbc42` | harness pinning the PG data-orphan guard probes and decision (RZ-3) |
| `dd8a81df6` | teach the worker-egress OpenRC runtime stub the typed container inspect (RZ-4 fallout) |
| `d2658df31` | startup fixture rejects the legacy bare inspect (RZ-4) |
| `ddc811811` | report — run-280 RZ-1..RZ-9 responses recorded |
| `e18d9a247` | report — F1 root cause reversed (harness gate discovery, not model) |
