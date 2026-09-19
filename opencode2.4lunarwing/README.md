# opencode2.4lunarwing

WebSocket bridge that integrates [OpenCode 2](https://opencode.ai)
(anomalyco/opencode, `v2` line) as a LunarWing external worker — the v2 lane
alongside the proven v1 worker (`opencode4lunarwing/`, pinned v1.18.25).
See `docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md` for the lane's rationale:
v1 stays the default; v2 is opt-in per tenant until upstream's 2.0.x patch
stack settles.

## Quick Start

### Docker

```bash
docker compose build
docker compose up -d lunarwing-worker
```

Build stage compiles one native OpenCode binary. Runtime image copies that
binary and the v2 client package source only; upstream monorepo and build
dependencies remain builder-only to keep image size and Kaniko memory use
bounded.

### CLI mode (one-shot)

The prompt is passed via the `TASK_PROMPT` environment variable:

```bash
TASK_PROMPT="hello world" docker compose --profile cli run opencode2-cli
```

## LunarWing Configuration

Add to `config.toml` under `LUNARWING_BASE_DIR` (mt-admin writes this via
`ensure_external_worker_config` when the worker is selected):

```toml
[[sandbox.external_workers]]
name = "opencode2"
url = "ws://localhost:9090/ws/agent"
timeout_ms = 3600000
```

The agent logs `External workers configured: opencode2` on startup.
`timeout_ms` defaults to one hour and is shared by all provider retries.
Every OpenCode SDK request receives the task abort signal. On timeout or
cancellation, the worker stops waiting immediately and gives session cleanup
a bounded grace (`OPENCODE_SESSION_ABORT_TIMEOUT_MS`, applied to
`session.interrupt`) instead of leaving work orphaned.
Transient provider streaming timeouts and temporary routing exhaustion are
retried twice in the same OpenCode session by default, preserving completed
workspace work.

## Differences from the v1 worker

| Area | v1 (`opencode4lunarwing`) | v2 (this worker) |
|---|---|---|
| Pin | v1.18.25 (proven) | v2.0.8 (beta in practice) |
| Client SDK | `@opencode-ai/sdk/v2/client` | `@opencode/client/promise` |
| Config file | `config/opencode.json` | `config/opencode2.json` |
| Config shape | `provider.<id>.{npm,options.baseURL}` | `providers.<id>.{package,settings.baseURL}` |
| Completion signal | `session.status` idle | `session.execution.{succeeded,failed,interrupted}` / `session.idle` |
| Text streaming | `message.part.updated` | `session.text.delta` |
| Cancel | `session.abort` | `session.interrupt` |
| Container UID | 10001 | 10003 |
| mt-admin ports | `opencode_wss` / `opencode_health` (extended block) | `opencode2_wss` / `opencode2_health` (dedicated range, registry v17) |
| Worker env overrides | `OPENCODE_MODEL` / `OPENCODE_BASE_URL` | `OPENCODE2_MODEL` / `OPENCODE2_BASE_URL` |

## Do not configure both lanes on one tenant during the transition

With both `opencode` and `opencode2` present the agent sees two `create_job`
modes and must guess which is proven. Keep v2 on scratch/eval tenants until
it earns trust (plan §5).
