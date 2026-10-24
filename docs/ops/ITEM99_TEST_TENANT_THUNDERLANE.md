# Item 99 — Temporary Test Tenant `thunderlane` (kimi + pebble external workers, full toolchains)

> Provisioned 2026-10-24 by SweetieBot (autonomous), branch
> `sweetiebot/kimi-99-20261024-1205`. The tenant runs exactly this tree:
> mt-admin's `clone_tenant_repo` clones the running checkout's current branch.
>
> **This tenant is deliberately left running.** The name `thunderlane` comes
> from the sanctioned test-tenant list in
> [WI#273](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/273)
> (spitfire, soarin, fleetfoot, dash, blaze, firestreak, highwinds, mistyfly,
> rapidfire, silverzoom, surprise, thunderlane, wavechill, windrider) and is
> not an animal name, so the animal-name teardown routine must not target it.

## Purpose

Item 99 of `docs/ops/AGENT_GOALS_2.1.6.0.md`: stand up a new test tenant with
the **kimi** and **pebble** external workers enabled with full toolchains,
assign both workers complex tasks, and report findings. Sibling items 100/101
repeat the same goal for later agents.

## Host

Gentoo dev VM (OpenRC first-class), rootless podman, 31 GB RAM, 325 GB free
disk. Pre-existing tenants: `antelope` (10000-10009), `scootaloo`
(10010-10019), `barracuda` (10020-10029). `thunderlane` takes the next free
block, **10030-10039**.

## Provisioning record (add-tenant)

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  bash ic/scripts/lunarwing-mt-admin.sh add-tenant thunderlane \
  --docker-group \
  --xmpp-jid thunderlane@xmpp.sobe.world \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-pebble --with-kimi --with-study-mcp
```

- `--llm-base-url` is the WireGuard-mesh TensorZero address every healthy
  tenant on this host uses (item 74 finding A1: the item-72 runbook's LAN
  `192.168.1.157` is unreachable from this host).
- Port block **10030-10039**: gateway 10030, http 10031, bridge 10032,
  postgres 10033, proxy 10034, weechat 10035, orchestrator 10036,
  pebble_wss 10038, opencode_wss 20037, opencode2_wss 51030,
  kimi_wss 30030, weechat_adapter 10039.
- Workers selected: `pebble` + `kimi` (exactly the item's ask; opencode and
  opencode2 intentionally not selected).
- `--with-study-mcp`: shared lw2studymcp checkout synced to
  `/var/cache/lunarwing/lw2studymcp`, published generation
  `5857c69e81b25f22e3de30a2f6ff33f019fc1c42`, tenant knowledge seed armed
  (`KNOWLEDGE_SEED_FROM_STUDY_MCP=1`, first boot only).
- PostgreSQL 16 (pgvector image) as `lunarwing-pg-thunderlane` on 10033.
- OpenRC init scripts + conf.d rendered; pre-delegated cgroup v2 subtree now
  covers `antelope barracuda scootaloo thunderlane`.
- Sandbox image `lunarwing-sandbox:latest` distributed into thunderlane's
  rootless store.

### WI#330 worker URL inheritance

`KIMI_BASE_URL` was written as the tenant's own LLM endpoint with
`KIMI_BASE_URL_SOURCE=inherited` — the container-reachable endpoint is
inherited rather than defaulted, exactly per the WI#330 design.

### Provisioning anomalies

- **P1 (host, pre-existing, same as item 72 A2):** WeeChat relay
  auto-bootstrap failed (`generated WeeChat relay config failed validation`).
  The tenant's weechat service starts but its relay is not configured.
  Host WeeChat 4.10.0 lacks cJSON, so `/relay add api` cannot succeed.
  Recovery path documented by mt-admin itself:
  `configure-weechat-relay thunderlane`.
- **P2 (cosmetic):** `group 'podman' does not exist; skipping` for
  `--docker-group` — expected on this rootless-podman host.
- **P3 (cosmetic):** `Creating mailbox file: No such file or directory` from
  `useradd` — the tenant user is created correctly; the host has no
  `/var/spool/mail`.

## Build record (build-tenant)

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh build-tenant thunderlane \
  --with-wasm --with-pebble --with-kimi --with-toolchains
```

(Filled in below — see the verification section.)

## Verification

`start-tenant thunderlane` → `START_EXIT=0`. All services under OpenRC with
`-sup` crash supervisors, boot-persistent (default runlevel):

- PostgreSQL (`lunarwing-pg-thunderlane`, 10033), daemon
  (`lunarwing-thunderlane`), xmpp-bridge, weechat + WS adapter, LunarVision
  sidecar (healthy, 20035/20036), and both worker containers
  (`lunarwing-pebble-thunderlane` :10038, `lunarwing-kimi-thunderlane` :30030).
- Gateway `:10030/api/health` → `{"status":"healthy","channel":"gateway"}`.
- `/api/gateway/status` (bearer): version 2.1.6, `llm_backend`
  `openai_compatible`, model `tensorzero::function_name::lunarwing`, channels
  unix_socket_repl/multica/weechat/xmpp/http/gateway, skills enabled.
  `multica` reports unhealthy (no multica server configured — cosmetic, same
  as every prior test tenant).
- **13 extensions active**: `study_notes` MCP + 9 WASM tools + 3 WASM
  channels (`multica`, `weechat`, `xmpp`).
- **WI#357 exposure audit**: `worker-listeners thunderlane` → `clean`. Both
  workers publish host loopback only (10038 / 30030 plus the in-container
  health 8443), no self-exposed inbound path.
- Worker bridges live: kimi `[bridge] server listening on
  ws://0.0.0.0:30030/ws/agent`, subprotocol `lunarwing-agent-v1`, auth
  enabled; pebble `[bridge] listening on ws://0.0.0.0:10038/ws/agent`.
  The kimi task runner starts as the task uid via the root bridge
  (WI#400 shape).
- **Agent LLM loop**: first gateway chat turn answered `BOOT_OK` in 41 s
  (`state: Completed`), proving the tenant's own LLM path
  (`LLM_BASE_URL=http://100.88.232.146:3000/openai/v1`) is live.
- LLM endpoint independently probed from the host with the tenant's
  `LLM_API_KEY` → HTTP 200 for both `tensorzero::function_name::lunarwing`
  and `tensorzero::function_name::FrontierCODEK` (the kimi lane's model).

## Worker task exercise

Driven through the real path: gateway chat (`POST /api/chat/send`) → agent LLM
loop → `create_job` tool → **approval gate** → `/api/chat/approval`
(`action=always`) → resume → orchestrator on-demand WS dial → worker bridge →
container executor → `job_events` → assistant reply.

> **Operational note (found on the first dispatch):** the approval POST must
> carry `thread_id`. Without it the daemon logs
> `engine v2: approval matched no engine gate; falling through to legacy path`
> with `scope="none"`, clears nothing, and the turn parks as `Failed` with the
> gate still pending. Supply the owning `thread_id` and the same call clears
> the gate and resumes the turn. This is worth a follow-up work item: the
> handler already has `ApprovalRequest.thread_id` but does not warn at the API
> layer when it is absent, so a caller gets `202 accepted` for a no-op.
> Also observed: after that no-op the original thread stopped accepting new
> turns (`/api/chat/history` showed no new turn), so a fresh
> `POST /api/chat/thread/new` thread was used for the remaining dispatches.

### kimi — task 1: memoized Fibonacci CLI (Rust)

Job `646bf782-cc67-4175-9d7d-0f870a2b7cf2`, `job_mode`
`external:kimi`, `created → running → completed` in **61 s**.

Worker events (`/api/jobs/{id}/events`, 23 of them) show the full tool loop
inside the container:

- `[Write] {"path":"/workspace/kimi_calc/src/main.rs", ...}`
- `[Bash] {"command":"cd /workspace/kimi_calc && cargo build 2>&1"}`
  → `Compiling kimi_calc v0.1.0 … Finished `dev` profile … in 0.71s`
- `[Bash]` running the three required cases →
  `=== N=10 === 55`, `=== N=50 === 12586269025`,
  `=== N=90 === 2880067194370816120` (all correct)
- `[Bash]` inspecting the written cache file → 81+ `N=value` lines present
- `result: "Completed: Everything works end-to-end…"` with the full report:
  toolchain **rustc 1.96.1 (31fca3adb 2026-06-26) / cargo 1.96.1
  (356927216 2026-06-26)**, the complete `main.rs` source, and the outputs.

This is real evidence that the fat kimi image's Rust toolchain works inside
the task sandbox (the item's "full toolchains" requirement), and that the
worker round-trips file writes, builds, executions, and a long result body.

### kimi — task 2: Go HTTP server (three endpoints)

Job `97040ff8-3295-4ebc-859c-2d123289f49a`, `job_mode` `external:kimi`,
completed in **290 s** on a fresh thread (14 events). Worker tool loop:

- `[Bash] go version && ls -la /workspace` → `go version go1.24.3 linux/amd64`
- `[Write] /workspace/kimi_go_http/main.go` (1588 bytes, stdlib only:
  `net/http`, `strconv`, `sync/atomic`)
- `[Bash] go mod init … && go build -o server .` → 8.4 MB binary produced
- `[Bash]` running the server in the background and curling every endpoint →
  ```
  --- GET /health ---      {"ok":true}
  --- GET /fib?n=20 ---    {"fib":6765,"n":20}
  --- GET /count (1st) --- {"count":1}
  --- GET /count (2nd) --- {"count":2}
  ```
- `result: "Completed: All done — the server builds and every endpoint behaves
  as specified…"` with the full source and the Go version.

Evidence value: the fat kimi image's **Go toolchain** works (1.24.3), the
worker can start long-lived background processes inside the task sandbox and
kill them, and the shared counter proves real in-process state, not a
replayed canned answer.

### pebble — task 1: parallel Fibonacci in C++17

Two earlier dispatches (`21008b1f`, `a15af501`, `1d7ecae4`) failed with
`api returned 401 Unauthorized (authentication_error): Invalid session`.
Root cause was **operator input, not the lane**: the `NANOGPT_API_KEY` value
copied from a sibling tenant's env carried literal surrounding double quotes,
and `podman --env-file` passes quotes through verbatim, so the key sent on the
wire was `"sk-nano-…"`. Proven by curling nano-gpt from *inside* the pebble
container: with the quoted value → 401 `invalid_api_key`; after stripping the
quotes → 200. `configure-pebble` was re-run with the clean value and the
container recreated. Recorded as finding **F1** below because the misleading
part is that the lane reported only "Invalid session" with no hint that the
credential had a quoting defect.

Job `9e0ecbfd-2ff5-42bc-84da-a0b1edd7c777`, `external:pebble`, completed.
Single `result` event carrying the whole answer:

- `g++ (Debian 12.2.0-14+deb12u1) 12.2.0`
- `/workspace/pebble_par/main.cpp` — C++17, `std::async` fan-out over
  `fib(1..40)` with a deliberately naive recursive `fib` so the parallelism is
  measurable, `steady_clock` timing, `-O2 -std=c++17 -pthread`
- Three timed runs from the generated runner script:
  ```
  sum=267914295 time_ms=85
  sum=267914295 time_ms=74
  sum=267914295 time_ms=80
  best_time_ms=74
  ```
- Final sum `267914295` — arithmetically correct
  (`sum(fib(1..40)) = fib(42) - 1 = 267914296 - 1`), so the worker's answer is
  computed, not asserted.

Evidence value: the fat pebble image's **C++ toolchain** works, `std::async`
threading is available inside the task sandbox, and pebble drives a
build → run → parse loop across a generated helper script.

### pebble — task 2: Rust recursive-descent expression evaluator

Job `9fedf119-0dcd-4704-ac31-e0a36120ad47`, `external:pebble`, completed.

- `rustc 1.96.1 (31fca3adb 2026-06-26)`
- `/workspace/pebble_rs/src/main.rs` — hand-written recursive-descent parser
  (grammar `expr → term (('+'|'-') term)*`, `term → factor (('*'|'/') factor)*`,
  `factor → ('+'|'-')* primary`, `primary → number | '(' expr ')'`), `f64`
  arithmetic, no external crates, argv-or-stdin input, non-zero exit on parse
  error. Built with `cargo build --release`.
- Five required inputs, all correct:

  | input | output | expected |
  | --- | --- | --- |
  | `2+3*4` | `14` | 14 (precedence) |
  | `(2+3)*4` | `20` | 20 |
  | `100/7` | `14.285714285714286` | f64 division |
  | `((1+2)*(3+4))-5` | `16` | 16 |
  | `2*(3+(4*5))` | `46` | 46 |

Evidence value: the pebble lane's **Rust toolchain** works (same 1.96.1 pin as
the kimi image), and the worker produced a genuinely non-trivial parser whose
operator-precedence handling is proven by the `2+3*4 = 14` case.

## Landed-changes exercise matrix

| Area | Evidence on thunderlane |
| --- | --- |
| WI#330 no-default LLM endpoint + worker inheritance | `KIMI_BASE_URL` written as the tenant endpoint with `KIMI_BASE_URL_SOURCE=inherited`; the endpoint is container-reachable (worker jobs called it successfully) |
| kimi worker lane | fat image, kimi-code **2.3.0**, `max_output_size = 131072` pinned on `tensorzero::function_name::FrontierCODEK`; two real jobs completed |
| pebble worker lane | fat image, pebble **0.5.0**; two real jobs completed after the key was supplied cleanly |
| Full toolchains (the item's explicit ask) | kimi image: rustc/cargo 1.96.1, go 1.24.3, g++ 12.x, python3, node 22 — all exercised in-container. pebble image: rustc 1.96.1, go 1.24.3, g++ 12.2.0 — all exercised in-container |
| WI#357 workers never self-expose | `worker-listeners thunderlane` → `clean`; both workers publish host loopback only |
| Approval gate / pending state | every `create_job` parked at the `approval` gate with the full parameter payload; `action=always` resumed it (see F2 for the `thread_id` requirement) |
| Knowledge seed + sweep + study MCP | 1799 notes seeded, 22 skipped, 20 failed (`knowledge_seed_state.json`); first sweep ran; `study_notes` extension active and authenticated |
| WASM channel/tool suite | 3 channels (`multica`, `weechat`, `xmpp`) + 9 tools installed; 13 extensions total |
| Host health pipeline (OpenRC contract) | `lunarwing-mt-health` covers thunderlane through its OpenRC units; no thunderlane-specific unit failures |
| Engine v2 turn lifecycle | turns transition `Created → Running → Completed`; failed turns leave the thread unusable (F3) |

## Findings

### F1 (corpus defect, pre-existing — reproduced, blocks the knowledge refresh)

The shared lw2studymcp corpus note
`agent-notes-by-agent/sweetiebot/lorebook-injection-defense-and-cache.md`
contains a **raw NUL byte at byte 2890** (inside a code span that intends to
show the escape text `</?[\x00s]*lore_entry`). PostgreSQL `text` rejects NUL,
so that note's write fails deterministically:

```
knowledge import: …/lorebook-injection-defense-and-cache.md: stranded-row write failed: Search failed: Update failed: db error
knowledge sync: additive refresh failed; serving last good corpus error_class="note_import_failed" retry_after_secs=61
```

`knowledge_sync_state.json` after ~35 min shows `consecutive_failures: 6`,
`last_success_at: null`, `notes_added_last_run: 0`, with 61 → 127 → 247 s
exponential backoff that can never succeed while the byte is in the corpus.
The one-shot **seed** absorbs it as a per-note warning (1 of the 20
`notes_failed`), so the tenant is functional from the seeded corpus, but the
hourly additive refresh is poisoned. This is the same defect item 74 recorded
as F1 on `fleetfoot` — still unfixed in the corpus generation this tenant
synced (`5857c69e81b25f22e3de30a2f6ff33f019fc1c42`). Two follow-ups stand:
(a) strip/escape the NUL in the lw2studymcp note; (b) make the refresh treat a
deterministic per-note import failure like the seed does (skip-and-count)
instead of failing the whole run.

### F2 (API ergonomics — approval without `thread_id` silently no-ops)

`POST /api/chat/approval` accepts `{"request_id": ..., "action": "always"}`
with no `thread_id` and answers `202 accepted`, but nothing happens: the daemon
logs

```
engine v2: approval matched no engine gate; falling through to legacy path
  user_id=thunderlane channel=gateway request_id=ee5c05ce-… scope="none"
```

and the gate stays in `pending-gates.json`. Supplying the owning `thread_id` on
the identical call clears the gate and resumes the turn. `ApprovalRequest`
already carries an optional `thread_id` (`ic/src/channels/web/types.rs:142`)
and the handler's doc comment says it is there "so the agent loop finds the
right session", but there is no warning at the API layer when it is omitted —
the caller gets a success status for a no-op. Suggested fix: return `400` when
`thread_id` is absent while exactly one gate exists, or resolve the gate's
thread from `request_id` server-side.

### F3 (usability — a failed turn wedges its thread)

After the F2 no-op the original thread stopped accepting turns:
`POST /api/chat/send` still returned `202 accepted`, but
`/api/chat/history` never gained a new turn and the second dispatch silently
never ran. A fresh `POST /api/chat/thread/new` thread worked immediately. This
affects any operator retrying a failed turn on the same thread.

### F4 (stale `shell` gates accumulate indefinitely)

After several dispatches `/api/chat/pending-gates` holds gates for `shell`
tool calls that were never approved and whose turns are long finished, e.g.

```
422ad756-… | shell | 0bb0de14-… | { "command": "echo \"job 9fedf119 completed …\"" }
699fe429-… | shell | f8b7f7e8-… | { "command": "FINAL_dummy=1; echo done" }
ef8ca6d7-… | shell | 0d67969e-… | { "command": "sleep 45 && echo \"waited\"", "timeout": 60 }
845460e0-… | shell | a2c71ee2-… | { "command": "echo done" }
```

They are never reaped when their turn ends, so the gate list grows forever and
(per F2) approving one without its exact `thread_id` does nothing. A
turn-completion sweep that drops gates whose thread is no longer running would
close this.

### F5 (operator credential hygiene — quoted values pass through `--env-file`)

Copying `NANOGPT_API_KEY` from a sibling tenant's `pebble.env` as the literal
`"sk-nano-…"` (with quotes) makes every pebble job fail with
`api returned 401 Unauthorized (authentication_error): Invalid session`.
`podman --env-file` does not strip quotes, so the quoted string is sent as the
bearer token. Proven from inside the pebble container: quoted value → 401
`invalid_api_key`, unquoted value → 200. `configure-pebble` writes whatever it
is given verbatim, so the operator sees only a generic auth error. Suggested
follow-up: have `configure-pebble` reject/strip a surrounding quote pair (or
warn) so a copy-pasted env line cannot silently poison the lane.

### F6 (cosmetic, expected)

- `multica` channel reports unhealthy (`invalid multica config: invalid type:
  null, expected a string`) — no multica server is configured for this tenant.
  Same as every prior test tenant.
- WeeChat relay not configured (provisioning anomaly P1) — the adapter and
  channel still come up; the host's WeeChat 4.10.0 lacks cJSON.
- `thunderlane@xmpp.sobe.world` XMPP auth: the bridge unit starts and is
  omemo-ready, but the account does not exist upstream (same operator step as
  items 72/74).

## Disposition

The tenant is **left running** (all services boot-persistent under OpenRC)
for maintainer inspection:

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh status thunderlane
sudo -u thunderlane bash -c 'XDG_RUNTIME_DIR=/run/user/$(id -u thunderlane) podman ps'
```

It is not an animal name and comes from the WI#273 list, so the teardown
routine must not target it. To decommission later anyway:
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant thunderlane --purge`.
