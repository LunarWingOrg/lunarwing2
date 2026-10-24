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

### pebble — complex task: parallel C++17 statistics engine

Job `4b44ed5a-6e4d-4e1c-8668-cc592bbc79fb` (`job_mode: external:pebble`),
`created → running → completed` in **182 s** (16:03:27 → 16:06:29). The job
description asked for a 20000-row deterministic CSV, a C++17 parallel
aggregator with `std::async`, deterministic merge, a reproducible checksum,
three identical runs plus 1/8-partition runs, and an independent awk/python
cross-check.

The worker produced a single long `result` event covering all six
requirements, with artifacts under `/workspace/item101_cpp`
(`gen_data.py`, `input.csv` 374600 bytes, `stats_parallel.cpp` 7917 bytes,
`stats_parallel` binary, five run-output files):

- `g++ (Debian 12.2.0-14+deb12u1) 12.2.0`, built with exactly
  `g++ -O2 -std=c++17 -pthread -Wall -Wextra`, no warnings.
- 20000 rows parsed, 0 malformed; FNV-1a checksum `0x88409e90bb3cd1b` over
  `fixed<<setprecision(6)` values.
- Per-category aggregates, identical across p=4 ×3, p=1 and p=8:

  | cat | count | sum | mean | min | max | stddev |
  | --- | --- | --- | --- | --- | --- | --- |
  | A | 5000 | -1074.064000 | -0.214813 | -499.944000 | 499.940000 | 287.968686 |
  | B | 5000 | -17428.120000 | -3.485624 | -499.709000 | 499.739000 | 289.271215 |
  | C | 5000 | -8552.960000 | -1.710592 | -499.950000 | 499.966000 | 291.791616 |
  | D | 5000 | 7426.040000 | 1.485208 | -499.735000 | 499.869000 | 290.240629 |

- Reported timings: p4 runs 0/0/0 ms, p1 0 ms, p8 1 ms (small dataset; the
  parallel path is exercised but not meaningfully benchmarked).

**Independent verification (not just the worker's claim):** re-ran the built
binary and an independent Python aggregation inside the pebble container with
`podman exec` — same checksum and byte-identical aggregates, and regenerating
`input.csv` with `gen_data.py` reproduces the same SHA-256
(`ec7b83a52bb91720a61f3aa0e7f3573756e1ba42c19838f7ece3f6f9a571f0bd`). The
worker's numbers are computed, not asserted.

### F3 (worker workspace lock rejects concurrent dispatches — works as designed)

Because two gateway turns were dispatched per lane while testing, each lane
received a second job while the first still owned `/workspace`. Both were
refused with a clear, actionable error rather than corrupting the workspace:

- kimi: job `85d1a0b4-d538-4f41-96e3-65f668f9f480` → `failed` after 105 s,
  `Failed: Workspace /workspace is already being modified by task
  7ae2cc44-… Cancel or finish that task before dispatching another task to
  this workspace.` Worker log: `rejected because workspace is owned by
  7ae2cc44-…`.
- pebble: job `0170b460-5db2-46e2-a502-aa322b7166d9` → `failed` in 34 s with
  the same message naming `4b44ed5a-…`.

So a single worker lane serialises on its workspace: only **one complex job at
a time per worker**. Operators must wait for the previous job to finish (or
cancel it) before dispatching the next one to the same lane.

### F4 (latency — upstream, not the lane): long turns take minutes

The complex kimi job's second LLM request took **508.8 s time-to-first-token**
(worker-side `kimi-code.log`: `turnStep=0.2 ttftMs=508781
streamDurationMs=8573 outputTokens=5668`), while the surrounding requests were
6 s and 6.1 s. During that window the only daemon-visible signal is the
two-minute `[worker] Still waiting for Kimi Code response; task remains
active.` heartbeat. A direct host-side probe of the same long prompt against
`FrontierCODEK` returned HTTP 200 in 48.6 s (300 max tokens), so this is
upstream queueing/latency on large requests rather than a broken bridge —
but a >8 minute silent stretch is indistinguishable from a hang to an operator.
Recommendation: surface per-request elapsed time in `job_events`, or add a
worker-side LLM timeout with a visible retry.

### F5 (gateway model output — `repl_argument_corruption` / `nested_fence`)

Long, code-heavy delegation prompts repeatedly drove the **gateway** model
(`tensorzero::function_name::lunarwing`) into emitting a REPL/tool draft with a
nested code fence, which the daemon rejects by design:

```
WARN Rejected corrupted REPL arguments before execution
  provider="tensorzero::function_name::lunarwing"
  error_class="repl_argument_corruption" corruption_signature=nested_fence
WARN model request failed after provider recovery failure_kind=repl_argument_corruption
```

Guard location: `ic/src/llm/stream_integrity.rs` (`validate_tool_response` →
`reject_repl_arguments`), corruption kinds in
`ic/crates/lunarwing_engine/src/types/repl.rs:18` (`nested_fence`). The
protection itself is correct — the tainted draft never executes — but two
operator-visible consequences matter:

1. The turn ends with **no assistant response** and no job, while
   `/api/chat/history` shows only `state: Failed` with `response: null` and no
   error text (F1). The reason is visible only in the daemon journal.
2. **It is not fully terminal:** one pebble-Go turn that logged
   `repl_argument_corruption` at 16:12:36 still created its job at 16:14:47
   (the model recovered and called `create_job` afterwards). Dispatchers must
   therefore wait for the *job*, not assume the turn is dead, or they will
   double-dispatch — which is exactly how jobs `e5c68890` (Go, rejected at
   16:17:06) and `85d1a0b4` (Rust) hit the F3 workspace lock.

Mitigation that worked here: adding to the *gateway* prompt "never emit
markdown code fences and never write Python REPL code yourself; put every
instruction in the create_job description as plain prose" made subsequent
delegations reach `create_job` promptly. A durable fix would be for the
delegation/system prompt to steer the model away from fenced REPL drafts when
the requested action is a single structured `create_job` call, and to surface
the failure kind on the turn so the operator does not have to read the journal.

### F6 (observability — no job id in the gateway response, no mode in job list)

`/api/jobs` returns only `id`, `title`, `state`, `user_id`, `created_at`,
`started_at`; `job_mode` (`external:kimi` / `external:pebble`) is available
only from `/api/jobs/{id}`. Multi-job testing therefore needs a detail call per
job to attribute it to a lane. The worker's own result is the authoritative
record of what ran.

### pebble — complex task 2: Go concurrent inverted index + HTTP server

Job `ab3ca985-763d-480b-8edf-3f51f8da5c85` (`external:pebble`), created
16:14:47, completed 16:19:36 (~289 s). Note this job was created **two minutes
after** its gateway turn logged `repl_argument_corruption` — see F5 point 2;
it is the same Go task that a later re-dispatch tried to duplicate.

Worker artifacts in `/workspace/item101_go`: `corpus.go` (50-doc seeded
corpus), `index.go` (4-goroutine pool + channels + mutex), `server.go`
(`/health`, `/doc`, `/search`, JSON 404), `main.go`, `index_test.go`, `go.mod`
with no external dependencies. Reported results, **all re-verified
independently** with `podman exec` on the container:

- `go version go1.24.3 linux/amd64`; `go vet ./...` clean.
- `go test -race ./...` → `ok item101_go 1.031s` (re-run: 1.032s); 5 tests
  (`TestBuildIndexConcurrent_Race`, `TestIndexPostingsAreSortedUnique`,
  `TestSearchHandler`, `TestNotFoundJSON`, `TestDocHandler`), all PASS under
  the race detector.
- `index` CLI: `documents=50`, `distinct_words=26`; `alpha:` 20 doc ids,
  `kilo:` 20, `zulu:` 18 — identical to the worker's report.
- Live server: `/health` → `{"ok":true}`; `/search?q=alpha` →
  `{"query":"alpha","count":20,"doc_ids":[0,1,2,5,6,8,9,17,19,21,23,24,28,29,32,36,39,42,43,47]}`;
  `/missing` → HTTP **404** with `{"error":"not found"}`.
- Worker-noted deviation: `ps` inside the container fails with `fatal library
  error, lookup self`, so it used `kill -TERM` + `wait` rather than `ps` as
  shutdown proof (matches F7 below).

### pebble — complex task 3: Rust expression evaluator with error handling

Job `39140d71-f9f8-45bf-a78a-5a982f8bd6b8` (`external:pebble`), created
16:19:51, completed 16:24:46 (**295 s**). `/workspace/pebble_item101_rs`,
no external crates, hand-written recursive-descent parser, f64, argv + stdin
modes. `rustc 1.96.1 (31fca3adb 2026-06-26)`, **5 unit tests passed**.

Independently re-run (`cargo test --release --offline` + the release binary):

| input | exit | output |
| --- | --- | --- |
| `2+3*4` | 0 | `14` |
| `(2+3)*4` | 0 | `20` |
| `100/7` | 0 | `14.285714285714286` |
| `((1+2)*(3+4))-5` | 0 | `16` |
| `2*(3+(4*5))` | 0 | `46` |
| `-(-3)` | 0 | `3` |
| `5/0` | 1 | `evaluation error at byte 1: division by zero` |
| `2+` | 1 | `parse error at byte 2: expected expression after '+'` |
| empty | 1 | `parse error at byte 0: empty input` |
| `1 2` | 1 | `parse error at byte 2: trailing garbage: unexpected '2'` |

stdin mode: three piped expressions printed `14` and `20`, then exited 1 with
`line 3: evaluation error at byte 1: division by zero`. Error paths really do
exit non-zero instead of panicking — the strongest evidence in this pass that
the lane handles non-happy-path requirements.

### F7 (minor, container environment): `ps` is unreliable inside the pebble task sandbox

The Go worker reported `fatal library error, lookup self` from `ps` in the
pebble container; host-side `podman exec … ps aux` works. Treat `ps` as
unusable from inside a pebble task and prefer `kill`/`wait`, `/proc`, or the
worker bridge's own status for process checks.

### kimi — complex task 2: Go HTTP server (three endpoints)

Job `352b152e-3ec4-4a1e-a5d8-09a09ecb180f` (`external:kimi`), created 16:30:53,
completed 16:34:36 (**223 s**, 29 events). This job ran **while the big Rust
job below still held `/workspace`**, and was accepted — so the F3 lock is not a
simple one-job-per-lane gate; see F8.

`/workspace/kimi_item101_go` (go.mod with no external requirements, main.go,
main_test.go). Raw tool results, not just the summary:

- `go version go1.24.3 linux/amd64`; `go vet` → `VET OK`;
  `go test ./...` → `TestFib` PASS, `ok kimi_item101_go 0.004s`; `BUILD OK`.
- One bash loop curled all seven endpoints and returned exact bodies/status:
  `/health` 200 `{"ok":true}`; `/fib?n=20` 200 `{"fib":6765,"n":20}`;
  `/fib?n=0` 200 `{"fib":0,"n":0}`; `/fib?n=bad` **400**
  `{"error":"invalid n \"bad\": must be an integer between 0 and 90"}`;
  `/count` twice → `{"count":1}` then `{"count":2}` (real in-process atomic
  state); `/unknown` **404**.
- Shutdown: the worker's first check showed the server still answering, then
  `pkill -x server` + a port check confirmed 8080 closed.
- Independently re-verified with `podman exec`: rebuilt the binary, reran vet/
  tests and the same curl series — identical bodies and status codes.
- Declared deviations: JSON key order is `{"fib":…,"n":…}` (Go maps marshal
  alphabetically), and the 400 body includes the offending value. Both are
  within spec.

### kimi — complex task 1 (large): Rust thread-pool expression evaluator — PARTIAL

Job `7ae2cc44-020c-47eb-a49e-28c01b5be812` (`external:kimi`), created
15:59:08. **Real, substantial work happened** (44 events by 16:17:21):

- `rustc 1.96.1 (31fca3adb 2026-06-26)` / `cargo 1.96.1 (356927216 2026-06-26)`.
- Created `/workspace/item101_rust` (Cargo.toml, src/main.rs), then iterated
  with `[Write]` and `[Edit]` on the parser and the bounded pool.
- `sample.txt` with 14 expressions including `2+3*4`, `-(2+3)*4`,
  `max(10, min(20, 15))`, `100/7`, `7%3`, `1/0`, `2+`, `((1+2)*(3+4))-5`.
- Ran the release binary twice: `TOTAL=14 OK=11 ERR=3 WORKERS_USED=4`, values
  correct (`2+3*4 = 14`, `-(2+3)*4 = -20`, `100/7 = 14.285714285714286`,
  `7%3 = 1`, `((1+2)*(3+4))-5 = 16`), error lines `1/0 ! division by zero`,
  `2+ ! expected number, identifier or '(' but found None at token index 2`,
  `100/(3-3) ! division by zero`; exit code 1 with errors mirrored to stderr.

**It did not finish.** After the 16:17:21 edit, `turnStep=0.17`'s LLM request
stayed unanswered for >15 minutes (F4/F9), leaving the crate mid-refactor. A
direct `podman exec` verification at that point is honest evidence of the
partial state:

- `cargo test --offline` and `cargo build --release --offline` **fail**:
  `src/main.rs:358:32` — `use of undeclared type AtomicUsize` (the worker's
  in-flight refactor removed the import while adding a `pending` counter).
  The previously built release binary still runs, which is why the outputs
  above are reproducible.
- Two consecutive runs **differ on stdout only in the `(wN)` worker-id
  suffixes** — e.g. `-(2+3)*4 = -20 (w1)` vs `(w2)`. Values, order and stderr
  are identical. The worker itself had already noticed this and was fixing it
  when the stall began: with per-result worker ids printed, the item-7
  "run it a second time to confirm deterministic output" requirement is not
  satisfiable byte-for-byte. That is a **task-design conflict** (spec asked for
  both per-result worker ids and byte-identical reruns), not a lane defect;
  a rerun comparing output with `(wN)` stripped is the right check.

### F8 (refinement of F3): the workspace lock is held by the *executing* task, not the lane

The duplicate dispatches in F3 were refused, but the later kimi Go job
`352b152e` was accepted and completed while the Rust job `7ae2cc44` still
owned `/workspace` (it created `/workspace/kimi_item101_go` alongside
`/workspace/item101_rust`). So the practical rule is: a second job is refused
while the first is **actively executing**, and accepted once the first is
idle/waiting. Concurrent jobs on one lane therefore share a writable
`/workspace`; the lock does not give per-job isolation over a long,
mostly-waiting task. Operators wanting isolation should still serialise jobs
per lane (or use separate task directories, as these did).

_(further tasks appended below)_
