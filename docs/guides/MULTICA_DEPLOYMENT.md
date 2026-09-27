# Connect LunarWing to Multica/Lunartica

LunarWing ships two registry extensions for Multica/Lunartica:

- the **`multica` WASM channel**, which polls for tasks, heartbeats, auto-registers a runtime when needed, and routes agent output back to the board;
- the **`multica-bridge` WASM tool**, whose agent-visible tool name is `multica` and which performs explicit runtime, task, issue, comment, and skill actions.

This integration is available on the current LunarWing source tree; it is not tied to the retired `1.1.0-new-lunartica` branch.

**Revalidated against source 2026-09-17 (v2.1.2.0).** The 30-second
`poll_interval_ms` clamp, token-presence polling gate, and channel startup
behavior were re-checked against `ic/channels-src/multica/src/lib.rs`;
referenced skill, manifests, and smoke-test scripts are present (the 2.1.2.0
release only bumped the channel crate/registry versions to 0.1.11; the
2.1.3.0 release again only bumped them to 0.1.12).

## Prerequisites

- A reachable Multica/Lunartica server and workspace UUID.
- A Personal Access Token accepted by that server, normally beginning with `mul_`.
- A unique daemon identifier for this LunarWing instance.
- A server URL allowed by the installed extension capability policy.

Use HTTPS for remote servers. Plain HTTP remote URLs are rejected by LunarWing's normal outbound URL safety policy unless the deployment explicitly enables private-network access.

## Install the Extensions

From the `ic/` checkout, build and install one or both registry entries:

```bash
lunarwing registry install multica --build
lunarwing registry install multica-bridge --build
```

Use the authenticated Extensions setup flow to provide the channel's required fields and `multica_api_token`. For the tool, `lunarwing tool auth multica-bridge` configures its declared token secret. The host injects the bearer token at the HTTP boundary; never write it into workspace JSON or a capability file.

Restart LunarWing after installing a channel or changing startup extension directories.

## Configure the Channel

The channel setup schema accepts:

```json
{
  "multica_url": "https://multica.example.com",
  "workspace_id": "<workspace-uuid>",
  "daemon_id": "lunarwing-prod-1",
  "runtime_type": "lunarwing",
  "polling_enabled": true,
  "poll_interval_ms": 30000
}
```

`poll_interval_ms` is clamped to a minimum of `30000`. On startup, the channel persists its scoped configuration, verifies that `multica_api_token` exists, and polls only when `polling_enabled` is true. If no runtime ID exists, it registers and stores the resulting ID in channel-scoped state.

The channel capability confines its workspace files below `channels/multica/`. Do not copy the tool's `config/multica.json` path into that directory manually; use extension setup so the host supplies the channel configuration in the expected shape.

## Configure the Tool

The `multica-bridge` tool reads `config/multica.json` from the agent workspace:

```json
{
  "url": "https://multica.example.com",
  "workspace_id": "<workspace-uuid>",
  "daemon_id": "lunarwing-prod-1",
  "runtime_type": "lunarwing"
}
```

Call the `multica` tool with `action: "register"`. Registration returns server JSON containing the runtime ID; add that value to the tool configuration before using runtime-scoped actions:

```json
{
  "url": "https://multica.example.com",
  "workspace_id": "<workspace-uuid>",
  "runtime_id": "<runtime-uuid>",
  "daemon_id": "lunarwing-prod-1",
  "runtime_type": "lunarwing"
}
```

The tool also supports legacy individual workspace keys such as `config/multica_url`, but the single JSON file is easier to audit.

## Tool Actions

The current capability manifest exposes:

| Action | Purpose |
| --- | --- |
| `register`, `heartbeat`, `recover_orphans` | Runtime lifecycle |
| `claim_task`, `start_task`, `complete_task`, `fail_task` | Task lifecycle |
| `report_progress`, `report_messages` | Execution reporting |
| `list_issues`, `get_issue`, `update_issue`, `post_comment` | Issue operations |
| `list_skills`, `get_skill`, `export_skill` | Board skill exchange |

The full polling lifecycle and required action order are defined in [`../../ic/skills/multica-poll/SKILL.md`](../../ic/skills/multica-poll/SKILL.md). Treat that skill and the extension manifests as the current parameter reference.

## Verify

1. Confirm `lunarwing registry info multica` and `lunarwing registry info multica-bridge` resolve the expected manifests.
2. Confirm the extensions are installed and the token setup reports configured without displaying its value.
3. For the channel, inspect logs for successful registration or heartbeat and verify the poll interval is at least 30 seconds.
4. For the tool, run `register`, save the returned runtime ID, then run `heartbeat`.
5. Use a disposable board task to validate claim, progress, and completion routing.

For local protocol checks, use `ic/scripts/multica-local-test-server.sh` and `ic/scripts/multica-bridge-smoke-test.sh`. Review those scripts before running them and keep test credentials separate from production.

## Troubleshooting

- **`multica_api_token secret not configured`** — complete channel setup or run `lunarwing tool auth multica-bridge`; exporting an environment variable alone does not place a token in the WASM secret store.
- **`runtime_id not set in config`** — the tool does not persist its registration response; add the returned ID to `config/multica.json`. The channel manages its own runtime state automatically.
- **Workspace or membership errors** — verify the workspace UUID and that the PAT's user belongs to that workspace.
- **No tasks arrive** — verify `polling_enabled`, the 30-second minimum interval, runtime assignment, and the channel's heartbeat response.
- **Outbound request rejected** — check the configured URL and installed HTTP capability allowlist. Do not weaken global URL validation merely to bypass a typo or an unexpected redirect.
