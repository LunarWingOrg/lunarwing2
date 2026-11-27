# LunarWing Gotify WASM Tool

A WASM tool for LunarWing that sends push notifications to a self-hosted Gotify server.

**Revalidated against source 2026-11-20 at `9c96220bb2`.** Registry manifest,
capability sidecar, and crate all use `0.1.10`. The compiled-in defaults
(`https://gotify.darkc.sobe.world`, title `LunarWing`) and the
`configure-gotify`/`--gotify-url`/`--gotify-title` mt-admin paths are the
starting point for the configuration below.

## Build and Install

The registry source is `ic/tools-src/gotify`, a symlink to the canonical `lunarwing-gotify-tool/` crate. Prefer the registry workflow from the `ic/` directory:

```bash
cd ic
lunarwing registry install gotify --build
```

`registry install --build` uses the manifest in `ic/registry/tools/gotify.json`, builds the WASM component from source, and installs its capability sidecar. Configure the token separately as described under [Secrets](#secrets).

For manual development builds, run `lunarwing-gotify-tool/build.sh`; it produces `lunarwing-gotify-tool/gotify.wasm`. Install both `gotify.wasm` and `gotify.capabilities.json` below `${LUNARWING_BASE_DIR:-$HOME/.lunarwing}/tools/`, then restart LunarWing.

## Configuration

### Gotify URL and Title

By default the tool sends notifications to `https://gotify.darkc.sobe.world` with the title `LunarWing`. Both are configurable via the workspace config.

**1. Workspace document** — create `config/gotify.json` in the agent's workspace (the database-backed workspace store, e.g. with the agent's `memory_write`):

```json
{"url": "https://your-gotify.example.com", "title": "My Agent"}
```

**2. On-disk operator config** — the host also reads `<LUNARWING_BASE_DIR>/workspace/config/gotify.json` at tool-execution time and merges it into the tool's workspace view. On a multi-tenant host `LUNARWING_BASE_DIR` is the tenant state directory, so this is where `lunarwing-mt-admin.sh add-tenant --gotify-url` and `configure-gotify` write the file. The on-disk file takes precedence over a workspace document: mt-admin rewrites the capability allowlist from that same file, so the effective URL and the allowlist always name the same host. Symlinked files and files over 64 KiB are ignored.

The tool appends `/message` automatically, so provide just the base URL. The `title` field is optional — if omitted, the compiled-in default (`LunarWing`) is used. The title serves as the default for notifications when the caller doesn't specify one explicitly.

**3. Capabilities file** — edit `gotify.capabilities.json` to allow HTTP to the new host. Update both the `allowlist` host and the `credentials.host_patterns`:

```json
{
  "capabilities": {
    "http": {
      "allowlist": [
        {
          "host": "your-gotify.example.com",
          "path_prefix": "/"
        }
      ],
      "credentials": {
        "gotify": {
          "secret_name": "gotify_app_token",
          "location": {
            "type": "header",
            "name": "X-Gotify-Key"
          },
          "host_patterns": [
            "your-gotify.example.com"
          ]
        }
      }
    }
  }
}
```

Both changes take effect on restart — no WASM rebuild needed. In a multi-tenant deployment, do not hand-edit either one: `configure-gotify` writes the workspace config and rewrites the capabilities host together.

If no config is found or it is unparseable, the tool falls back to the compiled-in defaults (URL and title).

### Provisioning Support

For a single-instance deployment, use the registry/setup flow above and write the reviewed workspace JSON and capability host allowlist explicitly. The old `ic/scripts/setup-instance.sh` path is deprecated in favor of `lunarwing onboard --quick` and the current extension lifecycle.

For a multi-tenant deployment, MT admin can configure Gotify during provisioning.
Replace the example LLM and Gotify URLs with your own endpoints:
```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant myagent \
  --llm-base-url https://llm.example.com/v1 \
  --gotify-url https://gotify.example.com --gotify-title "My Agent"
```
Writes `<tenant state>/workspace/config/gotify.json` at provisioning time. That is `<LUNARWING_BASE_DIR>/workspace/config/gotify.json`, which the daemon merges into the tool's workspace view on every execution. The capabilities file is automatically rewritten from the same URL when WASM tools are installed (`install-wasm`), and by `configure-gotify` immediately.

To reconfigure an existing tenant:
```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-gotify myagent https://new-gotify.example.com "My Agent"
```
Rewrites both the workspace config and the capability allowlist/credential host pattern, so the tool and the allowlist move together.

**Test harness** (`lunarwing-xmpp-test-env.sh`):

Set `LUNARWING_TEST_GOTIFY_URL` and optionally `LUNARWING_TEST_GOTIFY_TITLE` before running `install-wasm`. URL defaults to `https://gotify.darkc.sobe.world`.

**Environment variables:**

| Variable | Script | Description |
|---|---|---|
| `LUNARWING_MT_GOTIFY_URL` | `lunarwing-mt-admin.sh` | Default Gotify URL for new tenants |
| `LUNARWING_MT_GOTIFY_TITLE` | `lunarwing-mt-admin.sh` | Default Gotify notification title for new tenants |
| `LUNARWING_TEST_GOTIFY_URL` | `lunarwing-xmpp-test-env.sh` | Gotify URL for test harness |
| `LUNARWING_TEST_GOTIFY_TITLE` | `lunarwing-xmpp-test-env.sh` | Gotify notification title for test harness |

### Secrets

The tool requires one secret:

| Secret name | Description |
|---|---|
| `gotify_app_token` | Gotify application token (injected as `X-Gotify-Key` header by the host — never exposed to WASM) |

The shipped capability file declares the HTTP credential mapping, but its
`setup.required_secrets` list is empty. To enable the interactive setup prompt,
set this top-level section in the installed capability file (leave the rest
of the file intact):

```json
{
  "setup": {
    "required_secrets": [{"name": "gotify_app_token", "prompt": "Gotify application token"}]
  }
}
```

Then run `lunarwing tool setup gotify` with the target instance's configuration
and user scope (use the installed tool name if different). It reads the token
without echoing it and writes it to the encrypted store. Do not put the token
value in the workspace JSON or capability file.

## Usage

The agent calls the `gotify` tool with a JSON payload:

```json
{"message": "Deploy complete", "title": "CI", "priority": 8}
```

| Parameter | Type | Required | Default | Description |
|---|---|---|---|---|
| `message` | string | yes (schema) | `Notification from LunarWing` (guest fallback) | Notification body (supports markdown) |
| `title` | string | no | From `config/gotify.json`, or `LunarWing` | Notification title |
| `priority` | integer | no | `9` in the guest | Specify it explicitly: the published tool schema still advertises `3`. |

For routine prompts that use Gotify, see `ic/docs/GOTIFY_ROUTINE_PROMPT.md`.

## How It Works

The tool runs inside the WASM sandbox. It cannot read secrets or environment variables directly. The host runtime:

1. Reads `config/gotify.json` into the tool's workspace view when the tool calls `workspace-read` — the on-disk operator file (`<base_dir>/workspace/config/gotify.json`) first, then the workspace document
2. Injects the `gotify_app_token` into the HTTP request's `X-Gotify-Key` header at the host boundary
3. Validates the outbound URL against the capabilities allowlist before sending
