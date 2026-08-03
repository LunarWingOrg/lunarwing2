# kimicode4lunarwing

WebSocket bridge that integrates [Kimi Code CLI](https://github.com/MoonshotAI/kimi-code) (MoonshotAI/kimi-code) as a LunarWing external worker.

## Quick Start

### Docker

```bash
AGENT_AUTH_TOKEN="$(openssl rand -hex 32)"
export AGENT_AUTH_TOKEN
docker compose build
docker compose up -d lunarwing-worker
```

`AGENT_AUTH_TOKEN` is required in server mode, including loopback deployments;
Compose rejects a missing value before launching the container. `KIMI_API_KEY`
is optional when Kimi Code uses persisted OAuth credentials, but non-OAuth
providers still require it. Build with `WITH_TOOLCHAINS=true` for same Rust, Go,
C/C++, Python, and native development packages as OpenCode worker.

### CLI mode (one-shot)

The prompt is passed via the `TASK_PROMPT` environment variable:

```bash
TASK_PROMPT="hello world" docker compose --profile cli run kimi-cli
```

If `TASK_PROMPT` is unset, a default prompt is used.

## LunarWing Configuration

Add to `config.toml` under `LUNARWING_BASE_DIR`:

```toml
[[sandbox.external_workers]]
name = "kimi"
url = "ws://localhost:9090/ws/agent"
auth_token = "same value as AGENT_AUTH_TOKEN"
timeout_ms = 3600000
prompt_context = { max_messages = 12, max_chars_per_message = 2000, max_total_chars = 12000 }
```

The agent logs `External workers configured: kimi` on startup.
`timeout_ms` defaults to one hour. On timeout or cancellation, the worker
sends `SIGTERM` to the Kimi CLI subprocess, then `SIGKILL` after a bounded
five-second grace (`KIMI_KILL_GRACE_MS`) instead of leaving work orphaned.
LunarWing also sends a protocol cancel envelope when its own deadline expires.
Job metadata and progress events are persisted by LunarWing. After a daemon restart,
running jobs are reported as `interrupted`, while `list_jobs`, `job_status`, and
`job_events` can still read their saved status and event history from the database.
While inference is otherwise silent, the worker emits a persisted progress
heartbeat every 60 seconds; configure or disable it with
`KIMI_PROGRESS_HEARTBEAT_MS` (0-300000).
Only one active task may modify overlapping resolved workspace paths. A second
task targeting same directory, parent, or child fails immediately with active
task UUID; cancel or finish that task before dispatching another.

## Usage

```
create_job(title: "Fix the tests", description: "Run cargo test and fix failures", mode: "kimi")
```

Add `context_scope: "recent"` to include bounded user/assistant messages from
current conversation. Default `none` sends no history. History is untrusted
reference JSON; task remains authoritative. Metadata, identity, environment,
system prompts, and tool results never enter worker prompt. Configured limits
cannot exceed 50 messages, 10,000 characters per message, or 50,000 total.

External jobs default to `wait: false`. LunarWing returns the job ID immediately
and keeps monitoring worker events without blocking the current conversation
turn. Set `wait: true` only for short tasks that must return synchronously.

## Health Endpoints

- `GET /health` — Worker status, uptime, version
- `GET /ready` — WebSocket readiness and connection count

## Protocol

Speaks `lunarwing-agent-v1` WebSocket subprotocol (legacy alias `ironclaw-agent-v1` still accepted for one deprecation cycle). See `agent_comm_protocol.json` for the full spec.

## Environment Variables

See `.env.example` for the full list with descriptions.

## How It Works

The worker does not run a Kimi Code headless server (unlike the opencode worker which runs a persistent server). Instead, each task spawns a fresh `kimi -p <prompt> --output-format stream-json --yolo` subprocess:

1. The bridge receives a `task_request` envelope from LunarWing.
2. The executor spawns `kimi -p` with the assembled prompt (including bounded untrusted history).
3. Kimi Code CLI runs in print mode (`-p`) — no TUI, no human approval, auto permission policy.
4. stdout is a JSONL stream of `{role, content, tool_calls}` messages — the executor parses each line.
5. Assistant text and tool-call activity are streamed back as `task_progress` envelopes.
6. On process exit (code 0), the last non-empty assistant message is the canonical `task_result.output`.
7. On timeout/cancel, the subprocess receives `SIGTERM` then `SIGKILL` after a grace period.

This per-task-subprocess model means each task gets a fresh Kimi Code session (no cross-task state leakage), matching the isolation expectations of the LunarWing external-worker protocol.
