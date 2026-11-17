# opencode2.4lunarwing

WebSocket bridge that integrates [OpenCode 2](https://opencode.ai)
(anomalyco/opencode, `v2` line) as a LunarWing external worker — the v2 lane
alongside the proven v1 worker (`opencode4lunarwing/`, pinned v1.19.5).
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
`session.interrupt`) instead of leaving work orphaned. OpenCode v2.2.6's
noninteractive `opencode run` can wait indefinitely for background jobs. The
WebSocket bridge uses the serve SDK rather than `run`, and its task deadline
still interrupts a session whose events never settle; the executor regression
covers the new run-style `background_wait` shape without treating it as
completion. The optional `--mode cli` path does invoke `run`, so entrypoint.sh
bounds it with `OPENCODE_RUN_TIMEOUT_MS` (default `3600000`, one hour) and GNU
`timeout --preserve-status -k 5s`; the deadline sends SIGTERM through upstream's
normal background-job cancellation path instead of bypassing it.
Transient provider streaming timeouts and temporary routing exhaustion are
retried twice in the same OpenCode session by default, preserving completed
workspace work.

## Differences from the v1 worker

| Area | v1 (`opencode4lunarwing`) | v2 (this worker) |
|---|---|---|
| Pin | v1.19.5 (proven) | v2.2.7 (beta in practice) |
| Client SDK | `@opencode-ai/sdk/v2/client` | `@opencode/client/promise` |
| Config file | `config/opencode.json` | `config/opencode2.json` |
| Config shape | `provider.<id>.{npm,options.baseURL}` | `providers.<id>.{package,settings.baseURL}` |
| Completion signal | `session.status` idle | `session.execution.{succeeded,failed,interrupted}` / `session.idle` |
| Text streaming | `message.part.updated` | `session.text.delta` |
| Cancel | `session.abort` | `session.interrupt` |
| Container UID | 10001 | 10003 |
| mt-admin ports | `opencode_wss` / `opencode_health` (extended block) | `opencode2_wss` / `opencode2_health` (dedicated range, registry v17) |
| Worker env overrides | `OPENCODE_MODEL` / `OPENCODE_BASE_URL` | `OPENCODE2_MODEL` / `OPENCODE2_BASE_URL` |

WI#427 approved crossing the OpenTunnel boundary at
`843a6c514e606b436a7fe713422ace0a93ad3d61` (v2.0.32); the current v2.2.7 pin
is `fedc6f208aa2bd54c1019227214568083f6380ad` (tag resolved via git
ls-remote, every gate re-verified on the fresh clone).
The build compiles OpenTunnel out; the wrapper refuses service/pairing,
self-upgrade (`upgrade`/`update`), and non-loopback serving, and sharing
stays disabled. The wrapper also starts the binary with
`OPENCODE_FLAGS=-console_watches`, which keeps v2.2.7's Console event
watches off whatever the caller's environment or a workspace config says,
and the builder stage fails the image if the reviewed text behind that pin
stops holding (a text tripwire, not a proof; re-read the flag code on every
bump). The pin covers the event watches only: a worker that is given
an OpenCode Console credential, for example `OPENCODE_API_KEY`, still takes
Console-authored instructions and skills, plus the Console's config merge
of providers, remote MCP servers, policies and the web-search selection
(WI#665). The lane config's `update: "disable"` pin is not honored by
the updater itself (it reads only the global config dir), so the image
also carries `OPENCODE_DISABLE_AUTOUPDATE=1` to silence the background
check. See `docs/ops/WORKER-CONTAINERS.md` for the bump review, verification
and rollout record, including the open WI#369 SDK-gate and WI#424 egress
follow-ups.

## OpenCode Console credential boundary (WI#665)

The upstream `opencode` integration can fetch a hosted Console Library of
instructions and skills when it has both a Console credential and egress to
`opencode.ai`. At the pinned v2.2.6, its bundled catalog names
`OPENCODE_API_KEY`. The lane therefore refuses
to start when that variable is present, even empty, unless the operator sets
`LUNARWING_ALLOW_OPENCODE_CONSOLE_CREDENTIAL=1` (other conventional true/false
spellings are accepted). Acknowledging the credential does not make Console
instructions, skills, providers or MCP servers reach the session, and the
Console's own policy statements have no effect either: the managed policy below
withholds the content and outranks every Console statement. Startup messages
name the variable but never print its value.

This startup gate covers only the environment variable. Upstream prefers saved
credentials, including credentials saved by `opencode auth login` during a
session; they bypass this gate and may survive container restarts. The exposure
wrapper refuses `opencode auth login <http(s) url>`, which is the command that
would also register a well-known remote-config origin (WI#750), but a login
with an integration ID or name is still possible. The managed
Console deny below is the control that covers them.

## Managed Console policy (WI#665)

The image ships a root-owned, read-only managed policy at
`/etc/opencode/opencode.json` (source: `config/managed/opencode.json`):

```json
{ "policies": [
  { "action": "integration.use", "resource": "*", "effect": "allow" },
  { "action": "integration.use", "resource": "opencode*", "effect": "deny" },
  { "action": "integration.connect", "resource": "*", "effect": "allow" },
  { "action": "provider.use", "resource": "*", "effect": "allow" },
  { "action": "tool.use", "resource": "*", "effect": "allow" },
  { "action": "mcp.use", "resource": "*", "effect": "allow" },
  { "action": "skill.use", "resource": "*", "effect": "allow" },
  { "action": "websearch.use", "resource": "*", "effect": "allow" },
  { "action": "plugin.use", "resource": "*", "effect": "allow" }
] }
```

opencode loads `/etc/opencode` as managed settings above every project, global
and explicit document, and evaluates a managed document's policies first. The
deny therefore holds for a saved Console login, for a workspace `opencode.json`
that allows the integration, and for the real binary started without the
wrapper. It withholds Console instructions and skills, and the Console config
document's providers, remote MCP servers and web-search selection.

**WI#745 — Console statements have no effect.** Upstream keeps a policies-only
copy of a denied integration's document (`packages/core/src/config.ts:343-349`
at the pin), and those policies rank immediately below the managed ones. The
eight managed wildcard allows above therefore outrank every Console statement
for the actions upstream enforces at this pin, so a Console `integration.use`,
`integration.connect`, `provider.use`, `tool.use`, `mcp.use`, `skill.use`,
`websearch.use` or `plugin.use` deny no longer changes what the worker has or
can connect to. The `integration.use` wildcard allow MUST precede the
`opencode*` deny: the last match within a file wins. The evaluation includes
control/guarded pairs for wildcard and lane-provider `integration.use` denies,
and checks both provider and web-search catalogs alongside the Library tests.

**What else they outrank.** A managed allow outranks *every* lower layer's
deny for that action, not only the Console's, and it does so by design: there
is no way in upstream's policy model to rank Console statements separately. The
lane's ordinary permission rules still apply: a permission `deny` stays denied
and an `ask` stays `ask`, never turned into a grant. What is overridden is a
lower-layer `tool.use` policy deny, not a permission rule. All eight actions
use the same last-match rule from
`ManagedPolicy.statements`/`match` (`packages/core/src/managed-policy.ts`), so
the managed allow, loaded earliest, is the final match and wins. No upstream
patch is applied.

Managed settings outrank every other layer for ordinary settings too, so the
file carries `policies` and nothing else — an `experimental` or `model` key
there would shadow the lane's own config. `scripts/exposure_policy_assert.py`
enforces the exact nine-statement set and the integration allow-before-deny
order, the installed copy's owner and mode,
and that `/etc/opencode` holds nothing but that file: upstream also loads a
sibling `opencode.jsonc` from the managed directory, above `opencode.json`.

What it does **not** do is stop the requests. On a built image at the pin,
with a saved Console credential, the plugin still fetches
`/api/v2/config`, `/api/v2/instructions` and `/api/v2/skills`; the deny is
applied to what comes back. The tenant egress allowlist remains the only
control on outbound traffic. The evaluation that established this, and how to
repeat it, is in `docs/ops/WORKER-CONTAINERS.md` and
`scripts/console_policy_eval.py`.

See the [bump record](../docs/ops/WORKER-CONTAINERS.md#opencode-console-library-wi665)
for the conditions and the evaluation log.

## Do not configure both lanes on one tenant during the transition

With both `opencode` and `opencode2` present the agent sees two `create_job`
modes and must guess which is proven. Keep v2 on scratch/eval tenants until
it earns trust (plan §5).
