# Tenant Configuration Guide

This guide covers how to customize a LunarWing multi-tenant instance after it has been created by `lunarwing-mt-admin.sh`. It is written for both operators (who manage the host) and tenant users (who want to adjust their own settings).

## File Layout

Each tenant's configuration lives under their home directory:

```
/home/<tenant>/lunarwing/
├── env/
│   ├── lunarwing.env       # Main daemon and worker settings (mode 0600)
│   ├── pg.secret           # Stable PostgreSQL password (mode 0600)
│   ├── xmpp-bridge.env     # XMPP bridge service environment
│   ├── weechat.env         # WeeChat relay credential only (mode 0600)
│   ├── vision.env          # LunarVision sidecar environment
│   ├── proxy.env           # Present only when --enable-proxy is selected
│   ├── darkirc-adapter.env # Present only when --enable-darkirc is selected
│   ├── pebble.env          # Present after configure-pebble
│   └── gotify.json         # Gotify notification settings (optional)
├── state/
│   ├── config.toml         # Daemon settings (auto-seeded on first start)
│   ├── workspace-template/ # Identity files (SOUL.md, IDENTITY.md, etc.)
│   ├── tools/              # Installed WASM tools
│   ├── channels/           # Installed WASM channels
│   ├── darkirc/            # DarkIRC state when enabled
│   └── xmpp/               # OMEMO key store
├── logs/                   # OpenRC service log output
├── run/                    # Runtime sockets and PID files
└── ic/                     # Repo clone (source + build artifacts)
```

**Priority order**: environment variable > `config.toml` > database settings > compiled defaults.

After changing any env file, restart the affected service for changes to take effect (see [Applying Changes](#applying-changes)).

## LLM Provider Configuration

The LLM backend is configured in `env/lunarwing.env`. The per-tenant TensorZero
proxy is opt-in. Without `--enable-proxy`, provisioning derives TensorZero's
OpenAI-compatible `/openai/v1` surface from `LUNARWING_MT_TENSORZERO_URL`, or
uses an explicit `--llm-base-url` unchanged.

### Setting your API key

The most common post-setup task. Edit `lunarwing.env` and set:

```bash
LLM_API_KEY=sk-your-actual-key-here
```

If using TensorZero as a proxy, the key is passed through to the upstream provider. If pointing directly at an LLM API, this is the key for that provider.

### Switching LLM backends

The default setup connects directly to the configured upstream TensorZero URL:

```bash
LLM_BACKEND=openai_compatible
LLM_BASE_URL=http://tensorzero.example.com:3000/openai/v1
LLM_MODEL=tensorzero::function_name::lunarwing
```

Set the upstream root or `/openai/v1` URL with `LUNARWING_MT_TENSORZERO_URL`;
mt-admin adds `/openai/v1` when absent. Alternatively, set `LLM_BASE_URL`
directly at provisioning time with `add-tenant --llm-base-url <url>` (fleet-wide:
`LUNARWING_MT_LLM_BASE_URL`). When `--enable-proxy` is selected and no explicit
LLM base URL is supplied, mt-admin instead writes
`http://127.0.0.1:<proxy_port>/v1`.

To point directly at OpenAI (bypassing TensorZero):

```bash
LLM_BACKEND=openai_compatible
LLM_BASE_URL=https://api.openai.com/v1
LLM_API_KEY=sk-...
LLM_MODEL=gpt-4o
```

To point at Anthropic:

```bash
LLM_BACKEND=anthropic
LLM_API_KEY=sk-ant-...
LLM_MODEL=claude-sonnet-4-20250514
```

To point at a local Ollama instance:

```bash
LLM_BACKEND=ollama
LLM_BASE_URL=http://127.0.0.1:11434
LLM_MODEL=llama3
```

To point at any OpenAI-compatible endpoint (e.g. LiteLLM, vLLM, OpenRouter):

```bash
LLM_BACKEND=openai_compatible
LLM_BASE_URL=https://your-endpoint.example.com/v1
LLM_API_KEY=your-key
LLM_MODEL=your-model-name
```

### TensorZero proxy configuration

The proxy is disabled by default. Provision it with
`add-tenant <name> --enable-proxy` (the command is idempotent for an existing
tenant). mt-admin then
creates `env/proxy.env`, renders the init-specific proxy unit, and defaults the
daemon to that loopback endpoint:

```bash
PROXY_PORT=<allocated_port>
PROXY_BIND=127.0.0.1
TENSORZERO_URL=http://tensorzero.example.com:3000/openai/v1
```

`TENSORZERO_URL` is the upstream instance that the per-tenant proxy forwards to.
Change it through `--tensorzero-url` or `LUNARWING_MT_TENSORZERO_URL`, then
re-render/restart the tenant as appropriate.

## LunarVision Configuration

LunarVision defaults to a per-tenant local sidecar. Its local container settings
and stable local token live in `env/vision.env`; the daemon consumes
`VISION_SERVICE_URL` and `VISION_AUTH_TOKEN` from the mode-`0600`
`env/lunarwing.env`.

To move inference and OCR to a more capable machine, deploy LunarVision behind
an HTTPS reverse proxy, prepare a root-readable one-line file containing the
remote sidecar's `LUNARWING_AUTH_TOKEN`, then run:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-vision-external <tenant> \
  --url https://vision.example.com \
  --token-file /root/lunarvision.token
```

Remote URLs must be origins only: `https://host[:port]`, with no userinfo, path,
query, or fragment. The command verifies `/health` before switching, stops and
removes local LunarVision supervision, persists `vision.mode=external` and the
non-secret URL in `/etc/lunarwing/ports.json`, updates the daemon credentials,
and restarts the daemon when it is active. Both systemd-user and OpenRC tenant
lifecycles then treat the external instance as operator-managed. Host health
checks discover mixed local/external deployments directly from the registry.

Return to the local sidecar with:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-vision-local <tenant>
```

That command restores the local URL/token, recreates the health-gated container
and init supervision, and reloads the daemon. Do not hand-edit only one of the
environment files or the registry; use these commands so mode, credentials,
health discovery, and units change transactionally.

The external endpoint/token are deployment-specific and are deliberately not
carried by `export-tenant.sh`. After importing a tenant on another host, run
`configure-vision-external` again if that host should use the remote instance.
See [`../guides/VISION_OCR_SIDECAR.md`](../guides/VISION_OCR_SIDECAR.md) for the
remote ingress and endpoint contract.

## LunarVoice Configuration

LunarVoice (local sherpa-onnx speech-to-text sidecar) is opt-in per tenant and
managed through mt-admin:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh install-voice <tenant> --model-manifest <trusted.json>
sudo ic/scripts/lunarwing-mt-admin.sh activate-voice <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh deactivate-voice <tenant>   # restores prior transcription provider
```

Activation requires verified STT artifacts, publishes the registry
`voice_service`/`voice_health` ports, health-gates the sidecar, and sets
`TRANSCRIPTION_PROVIDER=lunarvoice` in the daemon environment. The gateway-side
voice API (desktop clients) is documented in
[`../guides/GATEWAY_VOICE_API.md`](../guides/GATEWAY_VOICE_API.md); sidecar
configuration and deployment live in `projects/voice-sidecar/README.md`.

## XMPP Configuration

XMPP settings in `lunarwing.env` control how the tenant connects to the XMPP network:

```bash
XMPP_JID=tenant@xmpp.example.com        # Tenant's XMPP account
XMPP_PASSWORD=...                        # XMPP account password
XMPP_RESOURCE=tenant                     # XMPP resource identifier
XMPP_DM_POLICY=allowlist                 # "allowlist" or "open"
XMPP_ALLOW_FROM=user@xmpp.example.com   # Comma-separated JIDs allowed to DM
XMPP_ALLOW_ROOMS=                        # Comma-separated room JIDs to join
XMPP_ENCRYPTED_ROOMS=                    # Rooms requiring OMEMO encryption
```

### Allowing direct messages

By default, `XMPP_DM_POLICY=allowlist` restricts who can message the agent.
Configure the authorized JIDs in `XMPP_ALLOW_FROM`:

```bash
XMPP_ALLOW_FROM=alice@xmpp.example.com,bob@xmpp.example.com
```

On a new mt-admin tenant, `XMPP_JID` defaults to `<tenant>@xmpp.localhost`
and `XMPP_ALLOW_FROM` defaults to `user@xmpp.localhost` (not the agent's
own JID). The bridge's JSON allowlist mirrors the daemon's list. Set a real
domain and authorized senders with `--xmpp-jid` and `--xmpp-allow-from` when
provisioning outside the local test XMPP network; existing values survive a
re-render. An explicit `--xmpp-allow-from` adds senders alongside the agent JID.

Set `XMPP_DM_POLICY=open` to allow messages from anyone (not recommended for production).

### Joining group chats

```bash
XMPP_ALLOW_ROOMS=room1@conference.xmpp.example.com,room2@conference.xmpp.example.com
```

For OMEMO-encrypted rooms, also add them to:

```bash
XMPP_ENCRYPTED_ROOMS=room1@conference.xmpp.example.com
```

### OMEMO encryption

```bash
XMPP_OMEMO_DEVICE_ID=0                  # Auto-generated on first run
XMPP_OMEMO_STORE_DIR=/home/<tenant>/lunarwing/state/xmpp
XMPP_ALLOW_PLAINTEXT_FALLBACK=true      # Set to false to require encryption
```

The XMPP bridge has its own env file (`xmpp-bridge.env`) that mirrors the JID, password, and bridge token. Both files must agree on `XMPP_BRIDGE_TOKEN`.

## Multi-Agent Coordination

Tenant provisioning writes coordination defaults to `env/lunarwing.env`:

```bash
COORD_ENABLED=false
COORD_AGENT_ID=<tenant-name>
COORD_AGENT_ALIASES=
COORD_RESPONDER_ROSTER=
COORD_CHANNELS=
COORD_ADMIN_TOKEN=<generated-secret>
COORD_ADMIN_PORT=<allocated-port>
```

Set `COORD_CHANNELS` to exact room JIDs, then set `COORD_ENABLED=true` and
restart tenant. Agent-ID aliases are derived automatically; add
`COORD_AGENT_ALIASES` only for extra names. To let an unaddressed `QUERY:` select
one responder across daemons, set the same comma-separated
`COORD_RESPONDER_ROSTER` on every participant, using each participant's exact
`COORD_AGENT_ID`. `add-tenant` preserves existing coordination values and token;
`patch-env <tenant>` backfills missing keys without rotating or printing token.
Agent IDs must be 1–128 ASCII characters using letters, digits, `@.-_/:+~`.
Transient XMPP thinking, tool, and reasoning statuses are suppressed in those
rooms by default. Set `COORD_XMPP_STATUS_UPDATES=true` only to show them.

## Gateway (Web UI)

```bash
GATEWAY_ENABLED=true
GATEWAY_HOST=127.0.0.1                   # Bind address (127.0.0.1 = local only)
GATEWAY_PORT=<allocated_port>
GATEWAY_AUTH_TOKEN=<auto-generated>       # Required for API/UI access
MASCOT_ENABLED=false                      # Opt in to the animated mascot
```

The web mascot is disabled by default. Set `MASCOT_ENABLED=true` in the tenant's
`lunarwing.env` and run `lunarwing-mt-admin.sh restart-tenant <tenant>` to load
its script and sprite routes. Full env re-renders preserve an existing choice;
older tenant envs gain the disabled default when they are next re-rendered.

To expose the gateway on all interfaces (e.g. behind a reverse proxy):

```bash
GATEWAY_HOST=0.0.0.0
```

The auth token is generated during tenant creation. To retrieve it:

```bash
grep GATEWAY_AUTH_TOKEN /home/<tenant>/lunarwing/env/lunarwing.env
```

## Engine V2 Channel Rollout

As of v2.0.6.0, multi-tenant provisioning writes Engine V2 into every
tenant's `lunarwing.env` with all eligible channels on by default:

```bash
ENGINE_V2=true
ENGINE_V2_CHANNELS=xmpp,weechat,darkirc   # default for new tenants
```

Existing tenants whose env lacks these keys gain the defaults on the next
upgrade/rebuild/re-render. An operator-set value is never overwritten:
`write_tenant_lunarwing_env` preserves both `ENGINE_V2` and
`ENGINE_V2_CHANNELS` across re-runs. An empty `ENGINE_V2_CHANNELS=` counts as
unset (the default returns on re-render). The only durable opt-out is
`ENGINE_V2=false`: a value like `ENGINE_V2_CHANNELS=none` is an ignored
unknown entry — the gateway and any allowlisted channels keep routing while
`ENGINE_V2=true`.

The gateway always uses Engine V2 when `ENGINE_V2=true`; other channels stay
on the legacy engine unless explicitly allowlisted. The only eligible
non-gateway names are `xmpp`, `darkirc`, and `weechat`. Entries are
comma-separated, trimmed, lowercased, and matched exactly; empty or unknown
entries do nothing. Listing `darkirc` is harmless for tenants without a
DarkIRC adapter — the allowlist only permits routing, it does not spawn
channels. `ENGINE_V2=false` disables Engine V2 for every channel regardless
of the allowlist.

Gateway responses stream incrementally. Current WASM channels ignore provider
chunks and receive one final response through their existing `on_respond`
contract, preserving the original JID/room, nick, or buffer metadata. No WIT
migration or WASM reinstall is required for this rollout.

Enable one configured channel at a time and restart through
`lunarwing-mt-admin.sh`. To roll back a channel, remove only its allowlist
entry and restart the tenant — the edited allowlist survives subsequent
re-renders. Setting `ENGINE_V2=false` immediately restores the legacy engine
for every channel without a database migration or state restore. Back up the
tenant env file before editing it; do not replace the rest of `env/` or
anything under `state/`.

### Engine V2 timeouts and model recovery

These optional overrides go in `env/lunarwing.env`. `lunarwing-mt-admin.sh
add-tenant` writes `ENGINE_V2_MODEL_RECOVERY_ATTEMPTS=2`,
`ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS=2` (WI#239) and
`ENGINE_V2_MAX_OUTPUT_TOKENS=8192` on the first render and preserves whatever
value is already there on re-render; the timeout is not written. The daemon
reads them from its process environment at startup, so restart the tenant
after adding or changing them.

```bash
ENGINE_V2_MAX_OUTPUT_TOKENS=8192        # Per-call output budget (max_tokens) for every Engine V2
                                        # model request; a fixed 4096 before 2.1.5.0. Thinking
                                        # models count their reasoning against it: if turns end
                                        # with "used its entire output budget before producing
                                        # visible text", raise it to 16384 or more. The adapter
                                        # retries an exhausted budget once at twice this value
                                        # before reporting that error. Values below 256 fall
                                        # back to the default.
ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS=300   # Per-future cap for a deferred tool or LLM call
                                        # awaited during an Engine V2 ResolveFutures round.
                                        # When the task exceeds this timeout it is aborted
                                        # (warn logged with timeout_type/call_id/action) and the
                                        # call resolves as an error instead of hanging the
                                        # thread forever. Default: 300 (5 minutes).
ENGINE_V2_MODEL_RECOVERY_ATTEMPTS=2     # Per-step model failure recovery attempts.
                                        # Provisioned as 2; the daemon's own default when
                                        # unset is 1; clamped to 0-3; 0 disables engine
                                        # recovery for every failure kind. When >= 1 it does
                                        # not clamp context_overflow (WI#298): its shrink
                                        # ladder changes the inputs instead of blindly
                                        # retrying, so it uses the policy cap (2); the
                                        # total-attempts cap below still bounds it.
ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS=2  # Total engine-level recovery attempts allowed
                                        # per thread lifetime. Bounds cumulative recovery
                                        # time. Provisioned as 2 (one full context-overflow
                                        # ladder uses it up); daemon default when unset is 4;
                                        # clamped to 16; 0 disables all engine recovery.
```

The defaults are correct for normal operation. Raise
`ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS` only if you observe delayed tool calls
(e.g. slow long-running builds invoked through tools) timing out at 5 minutes;
lower it if wedged tool futures should fail faster. Raise the recovery
attempts only if brief upstream model errors should be retried more
aggressively; note that supervised approval prompts are separately bounded by
the fixed 300s `supervised_timeout_secs` fail-closed auto-deny, which has no
env override.

## WASM Extensions

```bash
WASM_ENABLED=true
WASM_CHANNELS_ENABLED=true
WASM_TOOLS_DIR=/home/<tenant>/lunarwing/state/tools
WASM_CHANNELS_DIR=/home/<tenant>/lunarwing/state/channels
```

WASM tools and channels are installed into the tenant's state directory. Disable WASM entirely by setting `WASM_ENABLED=false`.

Set `TOOL_ACTIVITY=true` to route tool-activity status updates (`ToolStarted` / `ToolCompleted` / `ToolResult`) to the WASM channels. Off by default: the host drops those events before they reach a guest, so channels stay quiet. When enabled, the XMPP guest renders them in DMs and the DarkIRC/WeeChat guests emit them as `[status]` lines. Native channels (REPL, Signal) keep their own per-channel `/debug` toggles regardless of this flag.

## WeeChat Relay Configuration

During `add-tenant`, the WeeChat relay is configured automatically. The relay password is generated and stored as the literal expression `${env:RELAY_PASSWORD}` in `relay.conf`. The resolved value is provided through a dedicated `env/weechat.env` file (mode `0600`, owned by the tenant user) that contains only `RELAY_PASSWORD`. The full `lunarwing.env` is never loaded into the WeeChat process.

If the automatic bootstrap failed or was skipped before creating configuration, generate it with:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <tenant>
```

This command refuses to overwrite any existing non-empty WeeChat configuration (preserve-and-fail). On a fresh or empty target it generates a complete loopback-only relay config and writes the minimal `env/weechat.env`. See [`WEECHAT-SERVICES.md`](WEECHAT-SERVICES.md) for full details.

## Gotify Notifications

If a Gotify server is available, configure it in `env/gotify.json`:

```json
{
  "gotify_url": "https://gotify.example.com",
  "gotify_token": "your-app-token",
  "gotify_title": "TenantName"
}
```

The Gotify WASM tool reads this file at runtime. You can set the URL and title during tenant creation with `--gotify-url` and `--gotify-title`.

## Study Notes / Knowledge Seeding and Refresh

These env keys in `env/lunarwing.env` control self-knowledge wiring:

```bash
LW2STUDYMCP_HOME=/var/cache/lunarwing/lw2studymcp   # Root-managed, published checkout
KNOWLEDGE_SEED_FROM_STUDY_MCP=1                     # One-shot first-boot import
KNOWLEDGE_SYNC_ENABLED=true                         # Independent additive refresh cadence
KNOWLEDGE_SYNC_CADENCE_HOURS=6
KNOWLEDGE_SYNC_MAX_RECORDS_PER_RUN=500
KNOWLEDGE_SYNC_MAX_BYTES_PER_RUN=10485760
```

`add-tenant <name> --with-study-mcp` sets the checkout, seed flag, and bounded
sync defaults. The seed flag is written only on that provisioning path:
`configure-study-mcp` never re-arms it and `upgrade-tenant` never invents either
opt-in. Existing tenants enable refresh explicitly with
`configure-study-mcp <name> --sync --enable-knowledge-sync` and restart once.
Set the seed flag to `0` before first boot to suppress bootstrap, or use
`--disable-knowledge-sync` to stop periodic refresh.

The one-shot state is `state/knowledge_seed_state.json`; additive cadence,
revision, and failure/backoff state are in `state/knowledge_sync_state.json`.
The host publishes a validated Git generation hourly through a systemd timer or
OpenRC cron entry; tenant imports remain bounded, additive-only, and never
replace existing paths. Run an immediate pass with
`refresh-knowledge <name> [--sync-source]`. Full guide:
[`../guides/TENANT_KNOWLEDGE_SEED.md`](../guides/TENANT_KNOWLEDGE_SEED.md).

## Runtime Behavior

```bash
AGENT_NAME=tenant                        # Display name in conversations
CLI_ENABLED=false                        # Disable interactive CLI (daemon mode)
HEARTBEAT_ENABLED=false                  # Disable periodic background execution
ONBOARD_COMPLETED=true                   # Skip setup wizard
RUST_LOG=lunarwing=info                  # Log verbosity
LUNARWING_TOKIO_WORKER_THREADS=8         # Optional; default is min(host CPUs, 8)
LUNARWING_TOKIO_MAX_BLOCKING_THREADS=4   # Optional; default is min(host CPUs, 4)
```

Tokio's worker and blocking-pool caps keep idle memory stable on large hosts.
Increase them only after measuring sustained runtime saturation; they do not
limit Cargo or other subprocess parallelism.

Tool runtime estimation (WI#191) defaults to **observation-only**: the daemon
calibrates per-user/per-tool EMA runtime models in memory but shows nothing.
Set `LUNARWING_TOOL_ESTIMATES=1` before daemon start to opt into advisory
"typical runtime" hints in approval descriptions (after a five-sample warmup),
or `LUNARWING_TOOL_ESTIMATES=0` to disable both recording and hints. The value
is read once at startup, learned statistics reset on restart, and scheduling,
approval policy, timeouts, and budgets are never changed. Details:
[`../../ic/src/estimation/README.md`](../../ic/src/estimation/README.md).

The mt-admin env writer sets `AGENT_AUTO_APPROVE_TOOLS=false` on new tenants.
Re-rendering preserves an existing canonical override; an older tenant with
only `AUTO_APPROVE_TOOLS` has that value migrated to the canonical key.

### Lightweight routine tool rounds (WI#291)

`add-tenant` writes `ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=6` on the first render
and preserves an existing value on re-render; `upgrade-tenant` and `patch-env`
do not add it, so older tenants run with the daemon default of 3. The daemon
reads it at startup: restart the tenant after changing it (see
[Applying Changes](#applying-changes)).

A tool-enabled lightweight (v1) routine gets
`min(its max_tool_rounds, ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS, 10)` tool-calling
rounds, then one final text-only reply. The variable is a ceiling: it does not
lift a routine's own `max_tool_rounds` (3 unless set), so raising it alone
changes nothing for a routine still at 3. Tenant chats run on Engine V2 by
default (the gateway plus the opted-in XMPP, WeeChat and DarkIRC channels),
where the `routine_*` tools redirect to mission tools and cannot edit v1
routines. Raise one of the owner's routines from the host instead, as the
tenant, with the tenant's environment loaded by the safe loader mt-admin uses
for `refresh-knowledge`. It is invoked through `bash` and is init-agnostic
despite its name, so this works on systemd and OpenRC hosts:

```bash
sudo -u <tenant> bash /home/<tenant>/lunarwing/ic/scripts/lunarwing-openrc-env-exec.sh \
  --env-file /home/<tenant>/lunarwing/env/lunarwing.env -- \
  /home/<tenant>/lunarwing/ic/target/release/lunarwing --no-onboard \
  routines edit --name <routine> --max-tool-rounds <n>
```

`<n>` is 1–10; run `routines list` the same way to see the names. The binary
is under `target/release` unless `LUNARWING_MT_PROFILE` selected another build
profile. The command edits routines owned by the tenant's `LUNARWING_OWNER_ID`
and prints a note when the routine runs without tools, when tools are disabled
for lightweight routines, or when the cap is below `<n>`. Cron and manual
routines use the new value at their next fire, event routines after the next
event-cache refresh (about a minute); the daemon needs no restart. When a run
uses up its rounds, its final reply names the knob that bounded it (both, when
the routine's value equals the cap). Every round is one model call plus its
tools, so keep `ROUTINES_LIGHTWEIGHT_TIMEOUT_SECS` (default 300) in mind when
raising either value.

### Enabling heartbeat

The heartbeat system runs proactive periodic tasks. To enable:

```bash
HEARTBEAT_ENABLED=true
```

Then customize `state/workspace-template/HEARTBEAT.md` with instructions for periodic execution.

### Adjusting log verbosity

```bash
RUST_LOG=lunarwing=debug                 # More detail
RUST_LOG=lunarwing=trace                 # Maximum verbosity
RUST_LOG=lunarwing::agent=debug          # Agent loop only
```

## Identity Files

On first start, the daemon seeds `state/workspace-template/` with default identity files from `ic/deploy/workspace-template/`. These are injected into the agent's system prompt:

| File | Purpose |
|------|---------|
| `SOUL.md` | Core personality and behavioral guidelines |
| `IDENTITY.md` | Agent identity, name, and role |
| `AGENTS.md` | Agent rules and constraints |
| `USER.md` | Information about the primary user |
| `HEARTBEAT.md` | Instructions for periodic heartbeat execution |
| `TOOLS.md` | Tool usage guidance |
| `MEMORY.md` | Memory system configuration |

Edit these files to customize the agent's behavior for each tenant. Changes take effect on the next conversation session.

## config.toml

The `state/config.toml` file is auto-seeded on first daemon start. It provides defaults that can be overridden by environment variables. A minimal example:

```toml
llm_backend = "openai_compatible"
selected_model = "tensorzero::function_name::lunarwing"
openai_compatible_base_url = "http://tensorzero.example.com:3000/openai/v1"

[agent]
name = "tenant"
default_timezone = "America/New_York"
```

Environment variables always take precedence over `config.toml` values.

## Applying Changes

After editing configuration files, restart the affected service. The simplest,
init-agnostic way is the admin script, which restarts the whole tenant stack in
the correct order on either init system:

```bash
sudo lunarwing-mt-admin.sh restart-tenant <tenant>
```

For granular restarts, use the per-init commands below.

External workers have init-agnostic lifecycle commands. Prefer these over raw
`podman`: tenant containers use tenant-owned rootless stores, and their init
supervisors must be stopped before the container or they will respawn it.

```bash
sudo lunarwing-mt-admin.sh stop-worker <tenant> opencode
sudo lunarwing-mt-admin.sh start-worker <tenant> opencode
sudo lunarwing-mt-admin.sh restart-worker <tenant> opencode

# Emergency immediate stop. Disables supervision first, then uses a zero-grace
# tenant-scoped container stop and leaves the worker stopped.
sudo lunarwing-mt-admin.sh kill-worker <tenant> opencode
```

The same commands accept `pebble`, `kimi`, and `opencode2`. Running `podman stop` as root
targets root's unrelated store and runtime directory (`/run/user/0/libpod`), not
the tenant container. Do not use raw root Podman for tenant workers.

### Systemd

Per-tenant units are **user** units owned by the tenant's OS user, so they are
restarted on that user's bus — **not** with a system-level `systemctl restart`.
For each unit:

```bash
sudo -u <tenant> XDG_RUNTIME_DIR=/run/user/$(id -u <tenant>) \
  systemctl --user restart <unit>
```

The per-tenant units are:

```text
lunarwing-<tenant>.service                  # main daemon
xmpp-bridge-<tenant>.service                # XMPP bridge
lunarwing-proxy-<tenant>.service            # TensorZero proxy
lunarwing-weechat-<tenant>.service          # weechat backend
lunarwing-weechat-adapter-<tenant>.service  # weechat WS adapter
lunarwing-pg-<tenant>.service               # Postgres (rootless Quadlet)
lunarwing-pebble-<tenant>.service           # pebble worker (rootless Quadlet)
lunarwing-opencode-<tenant>.service         # opencode worker (rootless Quadlet)
lunarwing-opencode2-<tenant>.service        # opencode2 worker (rootless Quadlet)
lunarwing-kimi-<tenant>.service             # Kimi Code worker (rootless Quadlet)
```

There is no per-tenant `.target`; restart units individually, or use
`restart-tenant` above for the whole stack.

### OpenRC

Per-tenant units are system services under `/etc/init.d/`:

```bash
sudo rc-service lunarwing-<tenant> restart                  # main daemon
sudo rc-service xmpp-bridge-<tenant> restart                # XMPP bridge
sudo rc-service lunarwing-proxy-<tenant> restart            # TensorZero proxy
sudo rc-service lunarwing-pg-<tenant> restart               # Postgres container
sudo rc-service lunarwing-weechat-<tenant> restart          # weechat backend
sudo rc-service lunarwing-weechat-adapter-<tenant> restart  # weechat WS adapter
# workers (if provisioned): lunarwing-pebble-<tenant>, lunarwing-opencode-<tenant>, lunarwing-opencode2-<tenant>, lunarwing-kimi-<tenant>
# each worker also has a <unit>-sup babysitter; stop it before stopping a container manually
```

### Verifying

Check that the service started cleanly:

```bash
# Systemd
sudo journalctl -u lunarwing-<tenant>.service -n 20 --no-pager

# OpenRC
tail -20 /home/<tenant>/lunarwing/logs/lunarwing.log
```

## Operator Reference

### Creating a tenant with custom LLM settings

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant myagent \
  --llm-api-key sk-your-key \
  --tensorzero-url http://your-tz-host:3000/openai/v1 \
  --xmpp-jid myagent@xmpp.example.com \
  --gotify-url https://gotify.example.com
```

### Environment variable overrides for the admin script

These environment variables change the defaults used by `lunarwing-mt-admin.sh` when creating tenants:

| Variable | Default | Description |
|----------|---------|-------------|
| `LUNARWING_MT_TENSORZERO_URL` | (empty — required at `add-tenant` since WI#330) | Upstream TensorZero URL |
| `LUNARWING_MT_GOTIFY_URL` | (empty) | Default Gotify server URL |
| `LUNARWING_MT_GOTIFY_TITLE` | (empty) | Default Gotify notification title |
| `LUNARWING_MT_SOURCE_REPO` | (auto-detected) | Source repo to clone from |
| `LUNARWING_MT_PROFILE` | `release` | Cargo build profile |
| `LUNARWING_MT_WORKLOAD_SLICE` | `lunarwing-workloads.slice` | Slice for agent-spawned shell/compile scopes (BUG-005) |
| `LUNARWING_MT_WORKLOAD_MEMORY_HIGH` | `6G` | Workload slice throttling threshold |
| `LUNARWING_MT_WORKLOAD_MEMORY_MAX` | `8G` | Workload slice hard memory cap |
| `LUNARWING_MT_WORKLOAD_CPU_QUOTA` | `800%` | Workload slice CPU quota |
| `LUNARWING_MT_WORKLOAD_TASKS_MAX` | `2048` | Workload slice task cap |
| `LUNARWING_MT_BUILD_SLICE` | `lunarwing-build.slice` | Slice for operator-driven `build_tenant` cargo scopes |
| `LUNARWING_MT_BUILD_JOBS` | `$(nproc)` | Explicit `-j` passed to tenant cargo builds |
| `LUNARWING_MT_BUILD_MEMORY_HIGH` | `16G` | Build scope throttling threshold |
| `LUNARWING_MT_BUILD_MEMORY_MAX` | `24G` | Build scope hard memory cap |
| `LUNARWING_MT_DAEMON_MEMORY_HIGH` | `1536M` | Daemon unit throttling threshold |
| `LUNARWING_MT_DAEMON_MEMORY_MAX` | `2G` | Daemon unit hard memory cap |
| `LUNARWING_MT_DAEMON_TASKS_MAX` | `512` | Daemon unit task cap |

The workload/daemon caps exist so agent-spawned compiles are accounted and
bounded separately from the daemon (BUG-005). The daemon additionally reads
`LUNARWING_WORKLOAD_ISOLATION` (`auto`|`none`), `LUNARWING_WORKLOAD_SLICE`,
and `LUNARWING_WORKLOAD_MEMORY_MAX` from its own environment to control
transient-scope spawning (see `ic/src/tools/process.rs`).

### Port allocation

Each tenant gets a 10-port primary block from `/etc/lunarwing/ports.json`
(range 10000-19999), a mirrored 10-port extended block in 20000-29999
(`extended_base = base_port + 10000`), a two-port Kimi block in 30000-39999
(`kimi_base = base_port + 20000`), a two-port LunarVoice block in
40000-49999 (`voice_base = base_port + 30000`), and a two-port OpenCode 2
block in 51000-51999 (`opencode2_base = base_port + 41000`); the host-global
`health_metrics` slot lives at 50000. The current registry schema is v17.
Schema v14 gave every DarkIRC-enabled tenant a unique, stable, non-secret
`darkirc_scope_id`; v15 added the voice block, v16 the health_metrics slot,
and v17 the OpenCode 2 block. mt-admin creates or migrates that identity and
rejects invalid or duplicate values.

Primary block:

| Offset | Service |
|--------|---------|
| +0 | Gateway (web UI) |
| +1 | HTTP webhook |
| +2 | XMPP bridge |
| +3 | PostgreSQL |
| +4 | TensorZero proxy |
| +5 | WeeChat |
| +6 | Orchestrator |
| +7 | Reserved legacy Nanocode WSS slot |
| +8 | Pebble WSS |
| +9 | WeeChat adapter |

Extended block:

| Offset | Service |
|--------|---------|
| ebase+0 | DarkIRC adapter |
| ebase+1 | DarkIRC IRC |
| ebase+2 | DarkIRC RPC |
| ebase+3 | Reserved legacy Nanocode health slot |
| ebase+4 | Pebble health |
| ebase+5 | LunarVision service |
| ebase+6 | LunarVision health |
| ebase+7 | OpenCode WSS |
| ebase+8 | OpenCode health |
| ebase+9 | Coordination admin API |

Kimi block:

| Offset | Service |
|--------|---------|
| kbase+0 | Kimi WSS |
| kbase+1 | Kimi health |

LunarVoice block:

| Offset | Service |
|--------|---------|
| vbase+0 | LunarVoice STT sidecar API |
| vbase+1 | LunarVoice health |

OpenCode 2 block:

| Offset | Service |
|--------|---------|
| obase+0 | OpenCode 2 worker WSS |
| obase+1 | OpenCode 2 worker health |
