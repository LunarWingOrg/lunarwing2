# opencode4lunarwing

WebSocket bridge that integrates [opencode](https://opencode.ai) (anomalyco/opencode, formerly sst/opencode) as a LunarWing external worker.

## Quick Start

### Docker

```bash
docker compose build
docker compose up -d lunarwing-worker
```

Build stage compiles one native OpenCode binary. Runtime image copies that
binary and generated SDK client only; upstream monorepo and build dependencies
remain builder-only to keep image size and Kaniko memory use bounded.

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
`timeout_ms` defaults to one hour and is shared by all provider retries. Every
OpenCode SDK request receives the task abort signal. On timeout or cancellation,
the worker stops waiting immediately and gives session cleanup a bounded five-second
grace (`OPENCODE_SESSION_ABORT_TIMEOUT_MS`) instead of leaving work orphaned.
LunarWing also sends a protocol cancel envelope when its own deadline expires.
Job metadata and progress events are persisted by LunarWing. After a daemon restart,
running jobs are reported as `interrupted`, while `list_jobs`, `job_status`, and
`job_events` can still read their saved status and event history from the database.
Transient provider streaming timeouts and temporary routing exhaustion (`No provider
available`) are retried twice in the same OpenCode session by default, preserving
completed workspace work without resetting the overall task deadline. Configure
this with `OPENCODE_PROVIDER_MAX_RETRIES` (0-5) and
`OPENCODE_PROVIDER_RETRY_DELAY_MS` (0-60000). While inference is otherwise
silent, the worker emits a persisted progress heartbeat every 60 seconds;
configure or disable it with `OPENCODE_PROGRESS_HEARTBEAT_MS` (0-300000).
Only one active task may modify a resolved workspace path. A second task targeting
the same workspace fails immediately with the full UUID of the active task; cancel
or finish that task before dispatching another.

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

## Study Notes MCP

Set `LW2STUDYMCP_HOME` to an absolute lw2studymcp checkout visible inside the
container. Startup validates the canonical scripts, skill, SQLite schema, FTS5
table, and database integrity before injecting both:

- `<home>/skills` into OpenCode `skills.paths`
- a local `study_notes` MCP process running `study_notes_mcp.py`

Read-only remains default. For multi-tenant installs, configure the host checkout
through mt-admin, which emits Podman mounts for both Quadlet/systemd and
imperative/OpenRC paths:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-study-mcp TENANT \
  --home /srv/lunarwing/lw2studymcp
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant TENANT
```

Enable mutation tools for core LunarWing agents with separate state:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-study-mcp TENANT \
  --home /srv/lunarwing/lw2studymcp --allow-write
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant TENANT
```

OpenCode always mounts code and current state read-only at `/opt/lw2studymcp` and
`/var/lib/lw2studymcp`. Write tools remain core-only because OpenCode workers have
broad shell/edit permissions that could bypass MCP-only controls on an RW mount.
Workers see core writes on later reads. Rerun configuration without `--allow-write`
to disable core mutation tools while retaining state for reads.

Do not use `opencode_study.py` auto-clone inside the persistent worker. Runtime
network cloning would make worker content unpinned and startup network-dependent.
