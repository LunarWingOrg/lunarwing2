# opencode4lunarwing

WebSocket bridge that integrates [opencode](https://opencode.ai) (sst/opencode) as a LunarWing external worker.

## Quick Start

### Docker

```bash
docker compose build
docker compose up -d lunarwing-worker
```

### CLI mode (one-shot)

The prompt is passed via the `TASK_PROMPT` environment variable:

```bash
TASK_PROMPT="hello world" docker compose --profile cli run opencode-cli
```

If `TASK_PROMPT` is unset, a default prompt is used.

## LunarWing Configuration

Add to `config.toml` under `LUNARWING_BASE_DIR`:

```toml
[[sandbox.external_workers]]
name = "opencode"
url = "ws://localhost:9090/ws/agent"
timeout_ms = 3600000
prompt_context = { max_messages = 12, max_chars_per_message = 2000, max_total_chars = 12000 }
```

The agent logs `External workers configured: opencode` on startup.
`timeout_ms` defaults to one hour and is the hard task deadline enforced by both
LunarWing and this worker.
On timeout or cancellation, the worker aborts the active OpenCode session instead
of leaving it running in the container.
Job metadata and progress events are persisted by LunarWing. After a daemon restart,
running jobs are reported as `interrupted`, while `list_jobs`, `job_status`, and
`job_events` can still read their saved status and event history from the database.

## Usage

```
create_job(title: "Fix the tests", description: "Run cargo test and fix failures", mode: "opencode")
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
