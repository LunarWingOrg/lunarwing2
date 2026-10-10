# External Worker Tool Proxy (WI#324)

Status: phase 1 (hub-side) implemented. Worker-side bridge support is the
remaining piece and lives in the worker images (kimi / pebble / opencode).

## What this is

External workers are persistent, dispatch-tracked containers that hold a live
`lunarwing-agent-v1` WebSocket to the daemon for the duration of a job. WI#324
adds a **capability proxy over that control channel**: a worker can invoke a
allowlisted subset of the tenant's *in-process* tools (`memory_*`,
`knowledge_*`, `gitlab_tool`, …) without ever holding tenant credentials.

```
worker (CLI + thin bridge)  ──WS──▶  daemon external_worker  ──▶  ToolRegistry
      tool_call{name,args}      allowlist gate                 memory_* / knowledge_* / ...
      tool_result{...}          tenant-scoped JobContext       (worker never sees creds)
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

- `call_id` is required and echoed verbatim in the reply.
- `arguments` defaults to `null`; it is validated by the daemon's safety layer
  before execution.
- `task_id` is optional and **informational only**. Scoping always comes from
  the connection's job; a `task_id` that disagrees with the job this
  connection serves is a hard error.

### `tool_result` (daemon → worker)

```json
{
  "type": "tool_result",
  "payload": {
    "call_id": "<echoed>",
    "status": "success | error | denied",
    "output": "<tool result, pretty JSON string>",
    "error": null
  }
}
```

- `denied` is a **hard reject**: the tool was not in the worker's
  `tool_allowlist`. It is never a silent empty result.
- `error` covers malformed frames, task_id mismatch, an unconfigured proxy,
  an unknown (unregistered) tool name, and tool execution failure.
- The daemon always answers; a worker may block on the reply.

### `tool_list` (worker → daemon) → `tool_catalog` (daemon → worker)

`tool_list` takes an empty payload. The reply is:

```json
{ "type": "tool_catalog", "payload": { "tools": [ { "name": "...", "description": "...", "parameters": { } } ] } }
```

containing exactly the allowlisted tool definitions, so a worker-side MCP
bridge can expose a truthful `tools/list`.

## Capability allowlist (deny-by-default)

Per worker, in `config.toml`:

```toml
[[sandbox.external_workers]]
name = "kimi"
url = "ws://agent-hub:9000/kimi"
# phase-1 capability set from the WI: memory read+write, knowledge read+write,
# gitlab. mission_* stays gated until the mission lifecycle fixes land.
tool_allowlist = ["memory_read", "memory_write", "knowledge_search",
                  "knowledge_submit_note", "knowledge_get_topic", "gitlab_tool"]
```

- Empty or absent (the default) rejects **every** `tool_call`.
- Exact tool names opt in; `"*"` opts in to all registered tools (operator
  escape hatch, not recommended).
- The allowlist is the approval boundary for the proxy: in-process approval
  prompts cannot reach a worker, so listing a write tool (e.g.
  `memory_write`) is the operator's explicit grant.

## Execution and scoping

Allowed calls run through the shared `execute_tool_with_safety` pipeline
(lookup → validate → per-tool timeout → execute), with a `JobContext` built
from the **connection's** job: `job_id` of the running job and `user_id` of
the initiating user. That is what scopes `memory_*`/`knowledge_*` to the
originating tenant.

Calls are handled inline on the task's read loop (sequential per job in
phase 1). Each call carries the tool's own timeout; the job's overall
deadline bounds the whole loop, and cancellation resumes after the in-flight
call returns.

## Observability

Every proxied call writes two `job_events` records (`tool_call` on receipt,
`tool_result` with status/size/elapsed on completion) and a structured
tracing line. A worker-driven mission is therefore as inspectable via
`job_events`/`job_status` as an in-process one.

## Security invariants

- The proxy never exposes secret values. Secret injection remains exclusively
  on the `create_job(credentials=...)` path; `tool_call` can only return what
  the allowlisted tools return in the job's own tenant scope.
- Frame-supplied `task_id`/`user_id` are never trusted for scoping.
- Deny-by-default is enforced before the registry is even consulted.
- Workers remain untrusted containers: tool arguments pass through the same
  safety validation as in-process calls.

## Worker-side contract (remaining work)

A worker opts in by sending `tool_list` after `ready`, then issuing
`tool_call` frames and correlating replies by `call_id`. The reference plan
(WI#324 design item 2) is a thin stdio MCP server in the worker image that
translates MCP `tools/list`/`tools/call` into these frames, giving the worker
agent a native `mcp__lunarwing__*` surface with no changes to worker agent
logic. Phase 2 adds hub-mediated cross-thread coordination (KV + advisory
locks) for multi-threaded missions.

## Tests

- `ic/src/orchestrator/external_worker.rs` unit tests: allowlist gate
  (deny-by-default, exact, `"*"`), config gate parity, wire shapes.
- `ic/tests/external_worker_integration.rs`: mock-worker round trip over a
  real loopback WS — allowlisted call succeeds with tenant scoping asserted,
  non-allowlisted call is `denied`, empty allowlist denies even registered
  tools, unknown-but-allowlisted tool is an `error` (not a hang), and an
  unwired proxy replies `error` rather than dropping the frame.
