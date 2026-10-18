# External Worker Tool Proxy (WI#324)

Status: **partial**, hardened per the MR !1506 (CQ-1..CQ-9),
MR !1521 (DD-1..DD-10), MR !1533 (DP-1..DP-6), and MR !1540 (DX-1..DX-6)
reviews. The hub side is merged; the worker-side bridge (design item 2) is
implemented for the **kimi** worker lane (MCP shim) and the **opencode**
worker lane (serve-process plugin). Real-worker acceptance (AC1/AC2
against a live tenant with a rebuilt worker image) and the
opencode2/pebble lanes remain open. Cron-mode tick authority is
split out per MR !1506 CQ-10 (`cron.tool_allowlist`).

## What this is

External workers are persistent, dispatch-tracked containers that hold a live
`lunarwing-agent-v1` WebSocket to the daemon for the duration of a job. WI#324
adds a **capability proxy over that control channel**: a worker can invoke a
allowlisted subset of the tenant's *in-process* tools (`memory_*`,
`knowledge_*`, `gitlab_tool`, …) without ever holding tenant credentials.

```
worker (CLI + thin bridge)  ──WS──▶  daemon external_worker  ──▶  ToolRegistry
      tool_call{name,args}      allowlist + autonomous gates    memory_* / knowledge_* / ...
      tool_result{...}          tenant-scoped JobContext        (worker never sees creds)
```

## Wire protocol

All frames are the standard envelope `{id, type, timestamp, payload}`.

### `tool_call` (worker → daemon)

```json
{
  "type": "tool_call",
  "payload": {
    "task_id": "<job uuid>",
    "call_id": "<worker-chosen correlation id>",
    "name": "memory_read",
    "arguments": { "...": "..." }
  }
}
```

- `task_id` is **required** and must equal the id of the job this connection
  serves (MR !1506 CQ-7). Pooled sockets are reused across jobs; an unbound
  call could otherwise run in the next job's scope. Missing or mismatched
  `task_id` is a hard `error`.
- `call_id` is required and echoed verbatim in the reply.
- `arguments` defaults to `null`; it is validated by the daemon's safety layer
  before execution.
- Frames larger than 256 KiB are rejected with `error` before dispatch: the
  read loop first probes an oversized frame's `type` with a `Cow` field (so a
  type spelled with a JSON escape — `"\u0074ool_call"` — still classifies;
  serde_json cannot borrow an escaped string, MR !1533 DP-1), and re-checks
  the bound inside the `tool_call` arm so an oversized frame never reaches
  audit/persist/dispatch even after the envelope parse ran. The socket sets
  **both `max_message_size` and `max_frame_size` to 16 MiB** (DX-1/DX-3),
  bounding individual frame payloads and aggregate fragmented messages
  before JSON parsing. Exceeding either bound fails the job as a protocol
  error, without silent truncation. Other message types, including
  `task_result`/`task_progress`, may exceed 256 KiB within that 16 MiB
  transport ceiling, matching Kimi's configurable `WS_MAX_PAYLOAD_BYTES`
  maximum. See [worker limits](../ops/WORKER-CONTAINERS.md#websocket-message-limits).

### `tool_result` (daemon → worker)

```json
{
  "type": "tool_result",
  "payload": {
    "call_id": "<echoed>",
    "status": "success | error | denied",
    "output": "<sanitized tool result, pretty JSON string>",
    "error": null
  }
}
```

- `denied` is a **hard reject** on policy grounds: not in the worker's
  `tool_allowlist`, on the proxy denylist, blocked by the autonomous gates,
  `Disabled` in tool permissions, hook-rejected, or refused by the memory-write
  destination policy (engine state, prompt files, explicit layers). It is
  never a silent empty result.
- `error` covers malformed frames, missing/mismatched `task_id`, invalid
  identifiers, an unconfigured proxy, an unknown (unregistered) tool name,
  rate limiting, terminal job state, cancellation, and tool execution failure.
- `output` and `error` text — including denial/failure reasons built from
  hook or subsystem errors — pass through the SafetyLayer's
  `sanitize_tool_output` (size cap + LeakDetector redact/block + policy)
  before they are sent or persisted (MR !1506 CQ-2, MR !1521 DD-7).
- The daemon always answers; a worker may block on the reply. Calls arriving
  during a cancel handoff are answered with `error` (MR !1506 CQ-8) and are
  persisted as canonical `tool_result` rows too (bounded at 5 rows per
  handoff so a cancel-window flood cannot spam job_events).

### `tool_list` (worker → daemon) → `tool_catalog` (daemon → worker)

`tool_list` takes an empty object payload `{}` (a `task_id` field is accepted; when
present it must equal the connection's job id, same binding rule as
`tool_call` — a mismatch returns an error catalog; MR !1521 DD-10).
`tool_list` frames count toward the same per-job control-frame budget as
`tool_call`. The reply is:

```json
{ "type": "tool_catalog", "payload": { "tools": [ { "name": "...", "description": "...", "parameters": { } } ] } }
```

containing exactly the tools the worker could actually call: allowlisted,
registered, visible to the job's user, inside the phase-1 capability set and
not hard-denied (the catalog applies the same `is_proxy_callable` predicate
as dispatch, MR !1533 DP-6a), and not excluded by the autonomous gates
(MR !1506 CQ-6) — so a worker-side MCP bridge exposes a truthful
`tools/list`. A malformed `tool_list` payload (including `null` or a numeric `task_id`)
fails closed with an error catalog (MR !1533 DP-6b). An error reply keeps
the `tool_catalog` type with an empty `tools` array plus an `error` string.
Omitting `payload` fails the outer envelope parse; it is not a legacy catalog request.

## Capability allowlist (deny-by-default)

Per worker, in `config.toml`:

```toml
[[sandbox.external_workers]]
name = "kimi"
# NOTE: `tool_allowlist` requires a channel an on-path attacker cannot inject
# frames into. A cleartext `ws://` endpoint whose host is not loopback — e.g.
# `ws://agent-hub:9000/kimi` on a plain container network — has its whole
# allowlist stripped at load (see below). Use a loopback endpoint, or `wss://`
# for remote workers.
url = "ws://127.0.0.1:19000/kimi"
# phase-1 capability set from the WI: memory read+write, knowledge read+write,
# gitlab. mission_* actions are not part of this ToolRegistry surface.
tool_allowlist = ["memory_read", "memory_write", "knowledge_search",
                  "knowledge_submit_note", "knowledge_get_topic", "gitlab_tool"]
```

- Empty or absent (the default) rejects **every** `tool_call`.
- Exact tool names only. **There is no `"*"` wildcard** (MR !1506 CQ-1): a
  wildcard on an untrusted container is a full tenant-admin grant.
- At manager load the allowlist is sanitized, with each dropped entry logged:
  - `"*"` entries are removed;
  - names on the `AUTONOMOUS_TOOL_DENYLIST` (`create_job`, `job_prompt`,
    `restart`, `tool_install`, `skill_install`, `secret_list`,
    `secret_delete`, `ssh`, `ssh_git`, `routine_*`, …) are removed — they can
    never pass the execution gates;
  - names on the **proxy denylist** (`shell`, `read_file`, `write_file`,
    `list_dir`, `apply_patch`, `attachment_store`, `http`, `message`,
    `cancel_job`, `job_events`, `job_status`, `list_jobs`) are removed
    (MR !1521 DD-4, extended MR !1533 DP-2) — these are NOT on the engine's
    autonomous denylist, and without the proxy-level list they would pass
    every gate: an untrusted container would get command execution on the
    daemon host (direct exec when the Docker sandbox is unavailable),
    arbitrary host file reads/writes as the daemon user (the dev file tools
    have no `base_dir`; `validate_path` accepts any absolute path), host
    attachment-store access, credentialed outbound HTTP, or the
    cross-channel/job control and observation planes;
  - **names outside the phase-1 capability set are removed** (MR !1533 DP-2):
    the proxied surface is a *positive* list — `memory_read`, `memory_write`,
    `knowledge_search`, `knowledge_submit_note`, `knowledge_get_topic`,
    `gitlab_tool` / legacy `gitlab-tool`. `mission_status` is not a tool;
    `mission_history`/`mission_list` are Engine V2 bridge actions, not
    ToolRegistry tools, and are excluded (DX-6a). A denylist alone cannot deliver the phase-1 scoping:
    unlisted and future host tools would be exposed by default. The dispatch
    gate and the `tool_catalog` apply the same positive predicate on the
    resolved name, so a registry alias or a later-registered tool under an
    excluded name cannot slip through, and the denylist stays as defense in
    depth;
  - entries that are not valid tool identifiers are removed;
  - a worker reached over cleartext `ws://` on a **non-loopback** host has
    its whole allowlist stripped (MR !1506 CQ-7): tool authority must never
    ride a channel an on-path attacker can inject frames into. The host is
    classified with `http::Uri::host()` and fails **closed** on userinfo or
    unparseable URLs (MR !1521 DD-9): `ws://localhost:9000@host.example/x`
    connects to `host.example` and is correctly classified non-loopback. Use
    `wss://` or loopback.
- Worker requests must match an allowlist entry literally. The user-scoped
  registry then resolves it, and the resolved name must independently pass
  the phase-1 predicate. Prefer `gitlab_tool`: it also resolves a tool stored
  as legacy `gitlab-tool`. An exact `gitlab-tool` grant works when that name
  is registered; reverse alias resolution is not provided by the registry.
  Catalog entries retain the granted spelling so the bridge can call them
  (DX-6b). Persisted permission and autonomous gates apply to the resolved
  tool. Arbitrary hyphenated names such as `memory-write` remain excluded.
- The allowlist is the approval boundary for the proxy: in-process approval
  prompts cannot reach a worker, so listing a write tool (e.g.
  `memory_write`) is the operator's explicit grant. The seeded `AskEachTime`
  permission default is NOT enforced on this path (a worker cannot answer
  prompts); only a persisted `Disabled` permission still blocks.
- **Cron tick jobs never use the worker-level list** (MR !1506 CQ-10): a
  `mode = "cron"` mission fires unattended on a schedule, so tick jobs draw
  proxy authority from the mission's own `cron.tool_allowlist` (default
  empty — every proxied call from a tick is denied). The cron list goes
  through the same load-time sanitization (phase-1 positive set, denylist
  drops, cleartext strip).
- **A cron worker's worker-level list is stripped at load** (MR !1549
  EF-1): one worker container is **one authority domain** shared by every
  concurrent and successive job running in it — any same-uid process in the
  container can connect to another task's tool-proxy relay socket and act
  with that job's grant, and a detached leftover process can wait for later
  tasks' sockets. A worker-level `tool_allowlist` on a cron-mode worker
  would therefore be standing authority borrowable by every tick, so the
  manager drops it with an `error!` at load: the container never holds more
  than `cron.tool_allowlist`, and ordinary dispatches to a cron-mode worker
  get no proxied tools. Per-user isolation likewise requires per-user
  workers — jobs of two daemon users sharing one worker container share one
  authority domain. The strip is applied per worker entry, but the
  authority domain is the container: never point an entry that has a
  `tool_allowlist` at the same endpoint as a `mode = "cron"` entry; give a
  cron mission its own container. Two cron entries pointing at the same
  container likewise share one authority domain (one mission's tick can
  borrow the other's `cron.tool_allowlist` through another tick's relay
  socket, concurrent or later — a leftover process can wait for it), so
  the manager now detects that pairing at load (WI#396, MR !1582 review
  FT-3): cron entries are grouped by normalized endpoint key, and when a
  shared listener's entries carry *differing* `cron.tool_allowlist`
  grants the manager strips every cron grant on that listener with an
  `error!` (fail closed); identical grants are the same authority on
  both sides and stay, with a `warn!`. The manager additionally strips the
  worker-level list of a non-cron entry whose endpoint shares a cron
  entry's container (MR !1561 EV-1). The comparison uses a normalized
  endpoint key (MR !1571 review FE-1): loopback spellings fold into one
  class, the host is lowercased, the scheme-default port is applied, and
  the path/query are ignored — one host:port that path-routes several
  containers collapses to one key and strips all of them (fails closed).
  The key names a host:port listener, not the container itself (MR !1582
  review FT-2): a container also published on a second host port, or
  reached behind a proxy on another port, is not caught, and a real host
  literally named `loopback` folds into the loopback class (over-strips,
  fails closed). Those, and exotic aliases (`/etc/hosts` names), remain
  the operator's responsibility, so the rule stands on its own. A
  borrowed call is
  recorded in the `job_events` — and counted
  against the 100-frame budget — of the job whose relay socket was used,
  not of the job that issued it:

  ```toml
  [[sandbox.external_workers]]
  name = "kimi"
  url = "ws://127.0.0.1:19000/kimi"
  mode = "cron"
  # Do NOT set a worker-level tool_allowlist here: it is stripped at load
  # (a cron worker container must hold no standing interactive authority).

  [sandbox.external_workers.cron]
  schedule = "0 15 10 * * *"
  goal = "Produce the daily PR digest"
  # standing, unattended tick authority — grant the mission its own set:
  tool_allowlist = ["knowledge_search"]
  ```

  Rollback note (MR !1549 EF-6, corrected MR !1561 EV-5):
  `CronWorkerConfig` is `deny_unknown_fields`, so a binary older than this
  change cannot parse a config carrying `cron.tool_allowlist` — even `[]`
  from a settings write-back. The field is therefore serialized only when
  non-empty. What an older binary does with a config that still carries it
  depends on the source: with the **default** config path it boots anyway
  and ignores the whole `config.toml` (parse failure is only a `warn!`;
  `config/mod.rs`), a DB-stored workers array is skipped with a warning,
  and only an explicit `--config` path fails the boot. Either way the old
  binary could not honor a non-empty cron list, so the loss is confined to
  the case the field exists for.

## Execution gates (MR !1506 CQ-1)

An allowlisted call must additionally pass every gate the repo's other
autonomous paths (jobs, routines) enforce, in this order:

1. **Job context** — the real `ContextManager` context for the connection's
   job; terminal (`Cancelled`/`Failed`/`Accepted`) jobs are refused.
2. **User-scoped resolution** — `get_resolved_for_user`: legacy alias
   handling plus per-user visibility (another user's MCP/extension tools are
   NotFound, not leaked).
3. **Proxy capability gate** (MR !1521 DD-4, MR !1533 DP-2) — the resolved
   name must be inside the phase-1 capability set and not on the proxy
   denylist; enforced at dispatch (in addition to the load-time strip),
   regardless of the allowlist.
4. **Autonomous hard gates** — `allows_autonomous_execution()` and
   `ApprovalContext::is_blocked_or_default` with the allowlist as the
   approval grant. The allowlist cannot unlock denylisted or opt-out tools.
5. **Persisted `Disabled` permission** — a user who disabled a tool in tool
   permissions must not have it executed by a worker. Fails closed on store
   read errors.
6. **Memory-write destination policy** (WI#322, DX-2) — resolve `memory`,
   `heartbeat`, `daily_log` (also the omitted-target default), and `bootstrap`
   with the same mapping as `MemoryWriteTool`, then normalize the path.
   Refuse ALL of `engine/**` and anything classified by the workspace's
   `is_system_prompt_file`: identity/config files, `MEMORY.md`, `HEARTBEAT.md`,
   `BOOTSTRAP.md`, profile/directives, `lorebooks/**`, and `daily/*.md`.
   Workers cannot plant trusted engine state or persistent main-agent prompts.
   Any explicit `layer` parameter is refused, including empty/null values;
   ordinary notes such as `notes/proxied.md` use the owner's default scope.
7. **Per-tool rate limit** — the registry's limiter, keyed on the job's
   `user_id`.
8. **`BeforeToolCall` hooks** — operator hook policy, with sensitive params
   redacted; hook modifications are re-normalized. The memory-write policy
   is checked again on the final arguments before execution.

Approved calls execute the exact `Arc<dyn Tool>` instance the gates vetted —
validate → per-tool timeout → execute via `execute_timed_tool`, mirroring
`worker/job.rs`; there is deliberately no second registry lookup between the
gates and execution (MR !1521 DD-8) — scoped by the **connection's** job
(`job_id` + host-side `user_id`). Calls are handled inline on the task's
read loop (sequential per job in phase 1); the execution races the job's
cancel watch so a cancel is never queued behind a long tool timeout. Every
accepted call also refreshes the job's `last_activity_at` (MR !1521 DD-6) so
a tool-only worker is not mistaken for a wedged one by stuck-job detection;
the refresh happens before the capability gate, so allowlist-denied calls
count as activity too — bounded by the same 100-frame per-job cap (MR !1533
DP-6c).

A note on `knowledge_submit_note` (MR !1533 DP-6d): it is a phase-1 write
tool, and its `scope` parameter accepts `"shared"`, which writes the
herd-shared knowledge scope rather than the agent's own. Operators granting
it to a worker should treat that worker as able to plant notes other tenants
of the herd may read; withhold the entry if that is not intended.

A `memory_read` grant discloses the owner's workspace, including `USER.md`
and `MEMORY.md`, to a container with internet egress, subject to the tool's
reserved-path restrictions and output sanitization. The write policy above
does not restrict those reads (DX-2).

Compatibility: workers that previously wrote prompt files or selected a
memory layer now receive `denied`; use ordinary note paths for worker
reports. Reverting this policy restores persistent prompt-write authority
to the untrusted worker.

Per-job bounds (MR !1506 CQ-3, MR !1521 DD-5, MR !1533 DP-3): at most 100
proxied control frames per job — every `tool_call`-typed frame counts,
accepted, rejected, or malformed (a rejected-frame flood is capped too), and
`tool_list` shares the budget; worker `name`/`call_id` fields are validated
as bounded identifiers before they are persisted or echoed — including on
the cap path, which falls back to `unknown`/`""` for invalid values.
Rejection rows stop once the cap is crossed (one marker row is kept).

## Observability (MR !1506 CQ-4)

Every proxied call writes `job_events` in the **canonical** shape the web
renderer and orchestrator API already understand:

- `tool_use {tool_name, input}` when the call is accepted for dispatch
  (input previewed at 500 chars, redacted with the resolved tool's
  `sensitive_params` and leak-scanned first — worker-supplied arguments can
  never land a secret in job_events; MR !1521 DD-7);
- `tool_result {tool_name, success, output}` for every outcome — including
  denials, malformed frames, and calls answered during a cancel handoff,
  always with `success: false` so failed calls cannot render as green
  checkmarks. (A call whose execution future is dropped by the outer task
  timeout mid-run leaves its `tool_use` without a paired result — the job
  itself is recorded timed out in that window.)

Plus a structured tracing line per completed call.

## Security invariants

- The proxy never exposes secret values. Secret injection remains exclusively
  on the `create_job(credentials=...)` path; tool output and error text are
  sanitized (LeakDetector) before they reach the worker or `job_events`.
- Frame-supplied `task_id`/`user_id` are never trusted for scoping; the
  connection's job is the only scope.
- Deny-by-default is enforced before the registry is even consulted, and the
  autonomous hard exclusions apply even to allowlisted names.
- Workers remain untrusted containers: tool arguments pass through the same
  safety validation as in-process calls, and every worker-controlled field is
  bounded before persistence or logging.

## Worker-side contract (kimi and opencode lanes implemented)

A worker opts in by sending `tool_list` after `ready`, then issuing
`tool_call` frames (with `task_id` set to the job id from `task_request`)
and correlating replies by `call_id`. The kimi worker image implements this
as the WI's thin stdio MCP server, giving the worker agent a native
`mcp__lunarwing__*` surface with no changes to worker agent logic:

- `kimicode4lunarwing/scripts/lunarwing_tool_proxy.ts` — bridge-side relay.
  For each accepted task the bridge starts a per-task unix socket
  (`/tmp/lunarwing-tool-proxy/<id≤64>-<sha16>.sock` — the task id sanitized
  to `[A-Za-z0-9_-]` and capped at 64 chars, plus a 16-hex-char sha256
  suffix; directory overridable with `LUNARWING_TOOL_PROXY_DIR`, kept short:
  unix socket paths cap at ~108 bytes), injects `LUNARWING_TASK_ID` +
  `LUNARWING_TOOL_PROXY_SOCKET` into the CLI's environment (applied after
  task-supplied env so it cannot be shadowed), and routes inbound
  `tool_result`/`tool_catalog` hub frames back to the shim by `call_id`,
  scoped to the owning hub socket. The relay mirrors the hub's bounds —
  `[A-Za-z0-9_-]{1,128}` identifiers and a locally pre-checked 256 KiB
  `tool_call` frame cap — so an oversized call gets a clean local error
  instead of a hub-side job failure. Per-call timeout:
  `LUNARWING_TOOL_PROXY_TIMEOUT_MS` (default 300 s, capped 900 s). Task
  teardown fails in-flight calls; IPC is one-response-per-connection
  (end-after-flush) so teardown never drops a final reply. Proxy socket
  setup/teardown is best-effort (MR !1549 EF-2): the directory is a
  same-uid shared /tmp tree a task process can sabotage, so a setup failure
  degrades the task to tool-less instead of failing it, and teardown never
  throws out of slot cleanup. Closing the hub socket fails that socket's
  pending waiters immediately and drops its routing state (EF-9), and the
  socket stays dropped (MR !1561 EV-4): a later request on the still-open
  task IPC fails at once with the same closed-hub error instead of
  re-creating the pending map and waiting out the relay timeout. Note the
  hub runs proxied calls inline and serially (60 s default tool timeout), so
  several concurrent MCP calls from one task can outlast the 300 s relay
  timeout: the agent then sees a timeout for a call that later executes, and
  a retry duplicates the side effect. Workers should issue proxied calls
  sequentially.
- `kimicode4lunarwing/scripts/lunarwing_mcp_server.ts` — the stdio MCP
  server the kimi CLI spawns per task session. Implements `initialize`,
  `ping`, `tools/list`, `tools/call` over newline-delimited JSON-RPC; hub
  `parameters` maps to MCP `inputSchema`; `denied`/`error` tool results
  surface as MCP tool errors carrying the hub's sanitized reason. With no
  proxy env (cli mode, non-task runs) it advertises zero tools. Inbound
  lines are capped at 1 MiB (MR !1549 EF-9); an over-long line is discarded
  up to its terminating newline and answered exactly once with `-32600`
  carrying the request id recovered from the line's top-level edges
  (id-first or SDK order), so the CLI never waits out its own MCP timeout
  for a dropped call (MR !1561 EV-3).
  The grant is **ambient for every process in the task**, not just the MCP
  agent (MR !1549 EF-10): the socket path is in the CLI's environment, so an
  untrusted `npm install` or test script in the workspace can call the
  relay directly — and the kimi config loader merges
  `<gitroot>/.mcp.json` / `<cwd>/.kimi-code/mcp.json`, so workspace content
  can shadow or disable the `lunarwing` entry. Neither changes the security
  posture: the hub's allowlist and gates stay authoritative for every call.
- `entrypoint.sh` registers the shim in `$KIMI_CODE_HOME/mcp.json`
  (user-level, so no workspace trust prompt), write-if-absent so operators
  can disable or replace the entry.

The **opencode** lane implements the same wire contract with a different
in-task mechanism, because the kimi shim shape does not translate: `opencode
serve` is one long-lived process for every session and owns MCP servers per
project *Instance* with the serve process's frozen environment (verified
against the pinned opencode source, `packages/opencode/src/mcp/index.ts` @
`cb7d8b2f`), so no per-task socket env can reach an MCP child. Instead:

- `opencode4lunarwing/scripts/lunarwing_tool_proxy.ts` — the same
  bridge-side relay (identical IPC shapes, bounds, and never-throw
  lifecycle). The bridge starts/stops it around `executeTask` and routes
  `tool_result`/`tool_catalog` frames to it, exactly like the kimi lane.
- `opencode4lunarwing/scripts/lunarwing_tool_plugin.ts` — an opencode v1
  file plugin (registered by absolute path in `config/opencode.json`) that
  statically registers the phase-1 tools (`lunarwing_memory_read`, …,
  `lunarwing_gitlab_tool`, plus the legacy `lunarwing_gitlab-tool`
  spelling). Each tool's `execute()` receives the calling session's id,
  resolves the session title (`lunarwing-opencode-<task_id>`, set by the
  executor) through the plugin host's authenticated client, and relays the
  call over that task's proxy socket. Subagent sessions (titled
  `<description> (@<agent> subagent)` by opencode's `task` tool) resolve
  through the root session's title by walking `parentID`, bounded at 8 hops
  (JU-1). Successful resolutions are cached for the process lifetime —
  session ids are unique, and caching the FIRST observed title stops a
  mid-run rename after the first call from moving that session's calls.
  This is not an isolation boundary (JU-5): any process in the container
  can rename a session before its first call, create a session with another
  task's title — or a `parentID` pointing at another task's root session —
  over the loopback serve API, or connect to another task's socket
  directly. All of these stay inside the container's single grant (EF-1)
  and change only which job a call is attributed to. As with the kimi
  loader (EF-10), workspace content under `.opencode/` can shadow or
  disable the plugin: the effective config, including the plugin entry,
  lives in the model-writable `$WORKSPACE_ROOT/.opencode/opencode.json`,
  next to the auto-loaded `.opencode/plugin(s)` and `.opencode/tool(s)`
  directories; the hub stays authoritative. Sessions without the task
  title prefix (cli/acp modes, interactive use) get a clean error.
- The tools are dependency-free plain `{description, args, execute}`
  definitions (the registry's non-zod path, which passes call arguments
  through unvalidated), so the runtime image needs no `@opencode-ai/plugin`
  or zod. Only hub-REQUIRED arguments are declared — the registry marks
  every declared property required — with optional arguments documented in
  the description text; the hub's schema validation stays authoritative.
- Unlike the kimi lane's filtered `tools/list`, the static set is visible
  whether or not the worker has the grant; an ungranted call returns a tool
  error carrying the hub's denial reason (the hub stays deny-by-default and
  authoritative either way).

The opencode2/pebble lanes have no bridge yet; the same pattern
applies (their `lunarwing_bridge.ts` equivalents need the relay + frame
routing, plus a registration mechanism per lane). Phase 2 hub-mediated
cross-thread coordination (KV + advisory locks) is also still open.

## Tests

- `ic/src/orchestrator/external_worker.rs` unit tests: allowlist gate
  (deny-by-default, exact names, no wildcard), allowlist sanitizer
  (wildcard/denylisted/proxy-denylisted/non-phase-1/invalid dropped,
  cleartext strips all), the phase-1 capability predicate (positive set
  callable; host tools, stores, job planes, unlisted names, and mission
  actions denied), frame identifier validation, wire shapes, and a direct
  escaped `TypeProbe` parse that pins the `Cow` fallback independently of
  the in-arm bound (DX-5).
- `ic/tests/external_worker_integration.rs`: mock-worker round trips over a
  real loopback WS —
  - allowlisted call succeeds with tenant scoping asserted; non-allowlisted
    call is `denied`; empty allowlist denies even registered tools;
    unknown-but-allowlisted tool is an `error` (not a hang); unwired proxy
    replies `error` rather than dropping the frame;
  - AC1 hub-side evidence: real `memory_write` → `memory_read` round-trip through the proxy
    (LibSqlBackend workspace), plus the WI#322 engine-tree guard for both
    `engine/projects/**/missions/**` and `engine/projects/**` documents
    (MR !1521 DD-3, pinned per MR !1533 DP-5);
  - AC4: canonical `tool_use`/`tool_result` events asserted against a real
    store (`success: false` persisted for denied calls);
  - autonomous opt-out tool denied; autonomous-denylisted (`create_job`) and
    proxy-denylisted (`read_file`) entries never execute; persisted
    `Disabled` permission denied; terminal (Cancelled) job refused;
  - secret-shaped tool output never reaches the worker verbatim (CQ-2/AC5);
  - `tool_catalog` advertises only callable tools (CQ-6);
  - missing/mismatched `task_id` rejected (CQ-7);
  - MR !1533 DP pins: oversized `tool_call` frames rejected with `error`
    both plain and with a JSON-escaped `type` (DP-1), a frame past the 16 MiB
    socket cap fails the job as a protocol error (DP-1), and a 103-frame
    flood executes exactly 100 calls, denies the rest, persists exactly one
    cap marker row, and never persists/echoes unvalidated identifiers from
    the cap path (DD-5/DP-3).

- `ic/tests/external_worker_integration/proxy_review.rs` (DX regressions):
  - Results/progress over 1.5 MiB succeed with and without proxy grants and
    retain connection reuse; large worker error text is preserved;
  - fragmented messages below 16 MiB succeed; aggregate messages above it
    fail even though each fragment fits;
  - prompt destinations and aliases/defaults are denied without changing
    stored owner content; ordinary notes still round-trip; explicit layers
    and hook rewrites cannot bypass the policy;
  - canonical/legacy GitLab resolution, callable catalog spelling, ungranted
    calls, and persisted permission denials.

- `kimicode4lunarwing` (worker side, node): relay unit tests (frame shape,
  call-id routing scoped to the owning socket, identifier/frame bounds,
  task-close failure of in-flight calls, socket-path safety), shim MCP tests
  (initialize echo, catalog→MCP mapping, success/denied/transport-failure
  mapping, zero-tools fallback), and a full-path bridge test: a mock `kimi`
  binary drives the injected proxy env, the bridge emits `tool_call` on the
  hub WS, the mock hub's `tool_result` routes back, and the task result
  carries the tool output. Additionally verified against the **real kimi
  2.1.1 CLI** on a dev host (dead provider, no LLM spend): in print mode the
  CLI reads `$KIMI_CODE_HOME/mcp.json`, spawns the shim, passes the proxy env
  through to the MCP child, completes `initialize` →
  `notifications/initialized` → `tools/list` over newline-delimited JSON-RPC,
  and the shim's `list` op reached the relay socket.

- `opencode4lunarwing` (worker side, bun): relay unit tests (same suite as
  the kimi lane), plugin tests (the static surface is exactly the phase-1
  set with hub-required args only; happy-path relay with the title-resolved
  task id; denied → thrown tool error carrying the hub's reason;
  non-dispatched sessions error with zero hub traffic; first-title-wins
  caching), and a full-path bridge test: the real `handleTaskRequest` wiring
  (mock SDK) holds the task's relay socket live, an IPC call reaches the hub
  socket as a `tool_call`, `tool_result`/`tool_catalog` replies through the
  real `handleMessage` routing resolve the IPC callers, and the task's
  `finally` leaves zero relay tasks. Not covered locally: the plugin's load
  inside a real `opencode serve` (the image build gate runs the suites; the
  serve-side plugin registration needs the rebuilt image — the deploy gate).

Open acceptance items: AC1 with a **real** worker (hub-side evidence uses
mock sockets with real memory tools; worker-side evidence uses a mock CLI
against the real bridge/relay) and AC2 (`knowledge_search` real hits
through the proxy). Both need a rebuilt kimi worker image and a live
tenant, which is the protected-pipeline/deploy gate, not an MR-pipeline
lane; the opencode lane additionally needs its rebuilt image so the plugin
ships in `/app/scripts/`. The opencode2/pebble worker-side bridges are
also open. WI#324 stays open until those land.

## Land-order note (MR !1540 review #7716)

**!1541 (item 6 / WI#325) landed first** (target `84861d1b0`); this branch
back-merges it. The integration below is done in that merge commit:

- Keep both sides of the goals, `config/sandbox.rs`, `settings.rs`, and
  integration-test conflicts. Produce one `impl Default for ExternalWorkerConfig`
  and one `resolve_from_settings` literal carrying `mode`, `cron`, and
  `tool_allowlist`, retaining cron validation and both test sets.
- Change **both** `handle_tool_call` and `execute_proxied_call` cancellation
  parameters from `&watch::Receiver<bool>` to
  `&watch::Receiver<Option<CancelReason>>`. These breaks are outside conflict
  markers: the read loop and `wait_for_cancellation` have already changed
  in !1541 (previously !1531). The proxy select arm ignores the reason.
- Merge only on green `rust-check-*` and `rust-test-*` for the combined head
  (merging is not CI-gated).

Item 7's closed !1532 wiring conflict (`main.rs`, `orchestrator/mod.rs`)
needs both trailing `setup_orchestrator` dependencies (`workspace_resolver`
and `tool_proxy_deps`) in matching order. Item 8's closed !1537 overlaps the
goals, `external_worker.rs`, `tools/builtin/job.rs`, and integration tests;
retain both implementations and regression suites. Its old boolean cancel
watch must not undo the `Option<CancelReason>` integration after item 6.
