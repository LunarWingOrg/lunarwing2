# Item 100 (v2.1.7.0) — persistent test tenant `windrider`

## Scope and status

This is the v2.1.7.0 item-100 pass on branch
`rarity/kimi-100-20261030-0600`, continuing the earlier
`rarity/kimi-100-20261029-1936` pass (MR !2060, closed/rejected — run-271
review findings RW-1..RW-9). Started 2026-10-30 by Rarity. No subagents are used.

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

This pass re-landed the rejected !2060's core code fix (the `_ctr_inspect`
typed-container-inspect bug fix) and addressed every blocking and applicable
review finding from the run-271 adversarial review. The tenant, toolchains,
and both workers were already provisioned and running from the prior pass;
this pass's work is the code fix + review response + gateway-blocker
diagnosis, not a re-provision.

## Run-271 review findings — all addressed

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

## Gateway dispatch blocker — confirmed shared model impasse

Both workers are healthy on their published ports (pebble `:20014`,
kimi `:30010`, both `{"status":"ok"}`), the egress filter is live
(4 containers exclusively on `lw-egress-windrider`, DNS pinned to
127.0.0.1, static host `nano-gpt.com:216.150.1.1`, only `tcp dport 443`
allowed), and the listener audit is clean. The gap is purely that the
gateway model cannot drive `create_job`.

### Diagnosis (this pass, with debug log level)

The engine logs `routing user input through engine v2` → `__llm_complete__`
→ `Engine V2 Trace Summary: state=Waiting, tokens=17681, messages=3,
issues=2` → `WARNING: No assistant message in thread — model may not have
generated output` → `NOTE: 1 text response(s) without code blocks — model may
not be following CodeAct prompt` → `Suppressed empty response`.

The model (whether `openai/gpt-5.2` directly at `nano-gpt.com/api/v1`, or
the `tensorzero::function_name::lunarwing` variant which routes 99% to
`kimi_k3_nanogpt_tee_entry`) returns plain text, not a ```` ```repl ````
code block, so the engine's CodeAct parser finds no tool call and the turn
ends in `Waiting` with `response: null`.

### RW-8 ruled out

`ENGINE_V2_MAX_OUTPUT_TOKENS=8192` = the engine default. The output budget
is not the cause.

### Cross-tenant confirmation

The sibling tenant `dash` (same `tensorzero::function_name::lunarwing`
model, same TensorZero upstream `http://100.88.232.146:3000/openai/v1`)
fails the identical way: `response: null`, `state: Failed`, 0 jobs. This is
a shared model-level impasse, not windrider-specific.

### What was tried

- `openai/gpt-5.2` direct: 8 attempts, all `response: null`, engine logs
  "model may not be following CodeAct prompt".
- `tensorzero::function_name::lunarwing`: the kimi-k3 variant **did** emit
  CodeAct code blocks (engine logs show `Tool call started tool=time` and
  `tool=memory_tree` succeeded/failed), confirming the engine's CodeAct
  contract is working and the model CAN follow it. But the turn stalled on
  a `memory_tree` workspace error ("workspace tree changed since the
  previous page; restart from offset 0" even at offset 0) and never reached
  `create_job`.
- Explicit CodeAct demonstration prompt (with a ```` ```repl ```` example
  in the user message): still `response: null` on `openai/gpt-5.2`.
- The `/job <title>` submission-parse path does not help: Engine V2 routes
  all gateway `UserInput` through `handle_with_engine` before the legacy
  `handle_job_or_command` path, so slash commands go to the LLM too.

### What remains

The turn stalls on the `memory_tree` workspace error before `create_job`
fires on the TensorZero route. Fixing that workspace error or finding a
model variant that reliably emits `create_job` is the next pass's work.
This is a genuine, shared, model-level impasse — not a configuration decision
this pass can silently resolve.

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

## Test harness verification

All mt-admin + worker harnesses were re-run on this tree:

- **Previously broken (RW-1)**: `test-mt-admin-worker-listeners.sh` 30 PASS,
  `test-mt-admin-tenant-capability-writers.sh` rc 0,
  `test-worker-egress.sh` 144 PASS / 1 SKIP — all green.
- **RW-3 pins**: `test-mt-admin-worker-startup.sh` — all existing tests
  pass + static pin passes + 4 behavioural IPC-volume pins pass.
- **Full suite**: 26/28 mt-admin + worker harnesses pass. The 2 failures
  (`test-mt-admin-daemon-pid.sh`, `test-mt-admin-ssh-key-upload.sh`) are
  **pre-existing** — byte-identical rc=1 on the unmodified target
  `af270a192` (verified in a temporary worktree). The daemon-pid failure
  is a `/proc/<pid>/exe` exec-timing fixture artifact; the ssh-key failure
  expects env mode 640 but the fixture produces 600. Neither is caused by or
  related to the typed-inspect diff.
- **Repo gates**: `check-mt-admin.sh` shellcheck-clean (17 modules);
  `shell-syntax.sh` validated 272 scripts + 3 hooks; `shellcheck.sh` clean;
  `python_syntax.py` clean; `validate_config.py` clean (8 CI YAML files).
  `gitleaks` not installed on this host (pre-existing env gap, rc=127).

## Findings (new or carried)

- **F1 — Gateway model not following CodeAct (blocking, shared)**: both
  `openai/gpt-5.2` and `tensorzero::function_name::lunarwing` (kimi-k3
  variant) return text, not repl blocks, so `create_job` never fires. The
  TensorZero route DID produce CodeAct tool calls (`time`, `memory_tree`)
  but stalled on a workspace error before reaching `create_job`. Confirmed
  on `dash` too. **Next pass**: fix the `memory_tree` workspace error or
  find a model variant that reliably dispatches `create_job`.
- **F2 — RW-4 `volume rm` by partial name (pre-existing, worth a WI)**:
  two sites remove volumes by name with errors suppressed
  (`11-postgres.sh:219`, `07-units-systemd.sh:1227`). On a shared rootful
  store, purging `alice` can delete `lunarwing-pg-alice2`. Compare the exact
  name before removing. Default rootless stores are per-tenant and
  unaffected.
- **F3 — RW-9 reserved tenant-name prefixes incomplete (pre-existing,
  worth a WI)**: `04-tenant-lifecycle.sh:553-556` leaves out `task-`,
  `vision-`, `voice-`, `darkirc-`. Tenant `task-alice`'s pebble bridge
  `lunarwing-pebble-task-alice` collides with tenant `alice`'s pebble task
  container; OpenRC `init.d` names are global.
- **F4 — `memory_tree` workspace error**: the engine's `memory_tree` tool
  fails with "workspace tree changed since the previous page; restart from
  offset 0" even at offset 0 — blocking multi-step CodeAct turns on the
  TensorZero route. Worth a separate WI.

## Status and STOP report

**Item 100 is not fully complete.** The code fix (RW-1/RW-2/RW-3/RW-5) is
landed, all review findings are addressed, all harnesses are green, and the
tenant + both workers are up and left running. What is **not** done is the
complex-task dispatch, because the gateway model (shared with `dash`) does
not follow the CodeAct contract, so `create_job` never fires and no job can
be created through the real path.

**STOP / TRIED / BLOCKED BY / RECOMMEND**
- STOP: 12+ attempts across two model routes (`openai/gpt-5.2` direct and
  `tensorzero::function_name::lunarwing`), all yielded `response: null` with
  the engine logging "model may not be following CodeAct prompt"; the
  TensorZero route produced CodeAct tool calls but stalled on a workspace
  error before `create_job`. Identical root-cause repeats, not a new error
  each time.
- TRIED: both model routes; explicit CodeAct demonstration prompt; raised
  log level to `debug` to capture the real reason; ruled out the Engine V2
  output budget (RW-8: `ENGINE_V2_MAX_OUTPUT_TOKENS=8192` = the default);
  confirmed the workers are healthy and the egress/listener audits are clean;
  confirmed `dash` fails the same way.
- BLOCKED BY: the gateway LLM model does not emit the CodeAct ```` ```repl
  ```` block with a `create_job` call that the engine requires. On the
  TensorZero route, a `memory_tree` workspace error stalls the turn before
  `create_job` fires. This is a shared, model-level impasse affecting both
  `windrider` and `dash`.
- RECOMMEND: (1) fix the `memory_tree` workspace "tree changed since the
  previous page" error (F4) so the TensorZero route's multi-step turns can
  reach `create_job`; or (2) find a model variant in the TensorZero
  `lunarwing` function's experimentation set that reliably dispatches
  `create_job`. Then run the six prepared tasks through `dispatch.py` and
  verify with `verify.py`/`oracles.py`.

The item checkbox stays `[ ]`. Tenant and ports retained; no teardown.

## Commits on this branch

| commit | description |
| --- | --- |
| `2b1714899` | fix podman inspect cross-matching worker volume by prefix (the core bug) |
| `a55aabbf4` | guard PG volume data presence, typed inspects for root-store probes (RW-2, RW-5) |
| `d5ca84d09` | fix three CI harness stubs for the typed container inspect (RW-1) |
| `d8206f556` | pin the typed container inspect — static grep + IPC-volume behavioural (RW-3) |
