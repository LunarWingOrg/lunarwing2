# External Worker Tool Proxy (WI#324)

Status: phase 1 (hub-side) implemented, hardened per the MR !1506 (CQ-1..CQ-9)
and MR !1521 (DD-1..DD-10) adversarial reviews. Worker-side bridge support is
the remaining piece and lives in the worker images (kimi / pebble / opencode).

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
- Frames larger than 256 KiB are rejected with `error` before their payload
  is parsed: the read loop identifies an oversized `tool_call` with a
  zero-allocation borrowed type probe, so a huge frame never becomes a heap
  `Value` (MR !1521 DD-5). Other frame types are unaffected — a legitimate
  `task_result`/`task_progress` payload may be larger than a tool call.

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
  `Disabled` in tool permissions, hook-rejected, or refused by the WI#322
  engine-tree guard. It is never a silent empty result.
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

`tool_list` takes an empty payload (a `task_id` field is accepted; when
present it must equal the connection's job id, same binding rule as
`tool_call` — a mismatch returns an error catalog; MR !1521 DD-10).
`tool_list` frames count toward the same per-job control-frame budget as
`tool_call`. The reply is:

```json
{ "type": "tool_catalog", "payload": { "tools": [ { "name": "...", "description": "...", "parameters": { } } ] } }
```

containing exactly the tools the worker could actually call: allowlisted,
registered, visible to the job's user, and excluded by neither the proxy
denylist nor the autonomous gates (MR !1506 CQ-6) — so a worker-side MCP
bridge exposes a truthful `tools/list`. An error reply keeps the
`tool_catalog` type with an empty `tools` array plus an `error` string.

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
# gitlab. mission_* stays gated until the mission lifecycle fixes land.
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
  - names on the **proxy denylist** (`shell`, `write_file`, `apply_patch`,
    `http`, `message`, `cancel_job`) are removed (MR !1521 DD-4) — these are
    NOT on the engine's autonomous denylist, and without the proxy-level list
    they would pass every gate: an untrusted container would get command
    execution on the daemon host (direct exec when the Docker sandbox is
    unavailable), host file writes, credentialed outbound HTTP, or the
    cross-channel/job control planes. The runtime gate enforces the same
    list, so a registry alias or later-registered tool under a denied name
    cannot slip through;
  - entries that are not valid tool identifiers are removed;
  - a worker reached over cleartext `ws://` on a **non-loopback** host has
    its whole allowlist stripped (MR !1506 CQ-7): tool authority must never
    ride a channel an on-path attacker can inject frames into. The host is
    classified with `http::Uri::host()` and fails **closed** on userinfo or
    unparseable URLs (MR !1521 DD-9): `ws://localhost:9000@host.example/x`
    connects to `host.example` and is correctly classified non-loopback. Use
    `wss://` or loopback.
- The allowlist is the approval boundary for the proxy: in-process approval
  prompts cannot reach a worker, so listing a write tool (e.g.
  `memory_write`) is the operator's explicit grant. The seeded `AskEachTime`
  permission default is NOT enforced on this path (a worker cannot answer
  prompts); only a persisted `Disabled` permission still blocks.

## Execution gates (MR !1506 CQ-1)

An allowlisted call must additionally pass every gate the repo's other
autonomous paths (jobs, routines) enforce, in this order:

1. **Job context** — the real `ContextManager` context for the connection's
   job; terminal (`Cancelled`/`Failed`/`Accepted`) jobs are refused.
2. **User-scoped resolution** — `get_resolved_for_user`: legacy alias
   handling plus per-user visibility (another user's MCP/extension tools are
   NotFound, not leaked).
3. **Proxy denylist** (MR !1521 DD-4) — `shell`, `write_file`, `apply_patch`,
   `http`, `message`, `cancel_job` are hard-denied at dispatch (in addition
   to the load-time strip), regardless of the allowlist.
4. **Autonomous hard gates** — `allows_autonomous_execution()` and
   `ApprovalContext::is_blocked_or_default` with the allowlist as the
   approval grant. The allowlist cannot unlock denylisted or opt-out tools.
5. **Persisted `Disabled` permission** — a user who disabled a tool in tool
   permissions must not have it executed by a worker. Fails closed on store
   read errors.
6. **WI#322 engine-tree guard** — while that WI is open, proxied
   `memory_write` refuses targets under ALL of `engine/**` (MR !1521 DD-3,
   widened from missions-only): the engine reloads `engine/projects/**` and
   `engine/knowledge/**` from the owner workspace at boot, so a compromised
   worker could otherwise plant project or knowledge documents into the
   trusted in-process engine's store and prompts.
7. **Per-tool rate limit** — the registry's limiter, keyed on the job's
   `user_id`.
8. **`BeforeToolCall` hooks** — operator hook policy, with sensitive params
   redacted; hook modifications are re-normalized.

Approved calls execute the exact `Arc<dyn Tool>` instance the gates vetted —
validate → per-tool timeout → execute via `execute_timed_tool`, mirroring
`worker/job.rs`; there is deliberately no second registry lookup between the
gates and execution (MR !1521 DD-8) — scoped by the **connection's** job
(`job_id` + host-side `user_id`). Calls are handled inline on the task's
read loop (sequential per job in phase 1); the execution races the job's
cancel watch so a cancel is never queued behind a long tool timeout. Every
accepted call also refreshes the job's `last_activity_at` (MR !1521 DD-6) so
a tool-only worker is not mistaken for a wedged one by stuck-job detection.

Per-job bounds (MR !1506 CQ-3, MR !1521 DD-5): at most 100 proxied control
frames per job — every `tool_call`-typed frame counts, accepted, rejected,
or malformed (a rejected-frame flood is capped too), and `tool_list` shares
the budget; worker `name`/`call_id` fields are validated as bounded
identifiers before they are persisted or logged. Rejection rows stop once
the cap is crossed (one marker row is kept).

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

## Worker-side contract (remaining work)

A worker opts in by sending `tool_list` after `ready`, then issuing
`tool_call` frames (with `task_id` set to the job id from `task_request`)
and correlating replies by `call_id`. The reference plan (WI#324 design
item 2) is a thin stdio MCP server in the worker image that translates MCP
`tools/list`/`tools/call` into these frames, giving the worker agent a
native `mcp__lunarwing__*` surface with no changes to worker agent logic.
Phase 2 adds hub-mediated cross-thread coordination (KV + advisory locks)
for multi-threaded missions.

## Tests

- `ic/src/orchestrator/external_worker.rs` unit tests: allowlist gate
  (deny-by-default, exact names, no wildcard), allowlist sanitizer
  (wildcard/denylisted/invalid dropped, cleartext strips all), frame
  identifier validation, wire shapes.
- `ic/tests/external_worker_integration.rs`: mock-worker round trips over a
  real loopback WS —
  - allowlisted call succeeds with tenant scoping asserted; non-allowlisted
    call is `denied`; empty allowlist denies even registered tools;
    unknown-but-allowlisted tool is an `error` (not a hang); unwired proxy
    replies `error` rather than dropping the frame;
  - AC1: real `memory_write` → `memory_read` round-trip through the proxy
    (LibSqlBackend workspace), plus the WI#322 mission-path guard;
  - AC4: canonical `tool_use`/`tool_result` events asserted against a real
    store (`success: false` persisted for denied calls);
  - autonomous opt-out tool denied; denylisted entry (`create_job`) never
    executes; persisted `Disabled` permission denied; terminal (Cancelled)
    job refused;
  - secret-shaped tool output never reaches the worker verbatim (CQ-2/AC5);
  - `tool_catalog` advertises only callable tools (CQ-6);
  - missing/mismatched `task_id` rejected (CQ-7).

Open acceptance items: AC2 (`knowledge_search` real hits through the proxy)
and the worker-side bridge itself (design item 2) — the wire contract above
is the bridge's specification.
