# External Worker Tool Proxy (WI#324)

Status: **partial (hub side)**, hardened per the MR !1506 (CQ-1..CQ-9),
MR !1521 (DD-1..DD-10), MR !1533 (DP-1..DP-6), and MR !1540 (DX-1..DX-6)
reviews. Worker-side bridge support remains unimplemented in the worker
images (kimi / pebble / opencode); real-worker acceptance is still open.

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

Open acceptance items: the worker-side bridge (design item 2), AC1 with a
real worker (current evidence uses mock sockets with real memory tools),
and AC2 (`knowledge_search` real hits through the proxy). WI#324 stays open;
the wire contract above is the bridge's specification.

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
