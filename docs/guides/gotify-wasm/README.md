# LunarWing Gotify WASM Tool

A WASM tool for LunarWing that sends push notifications to a self-hosted Gotify server.

**Revalidated against source 2026-09-17 (v2.1.2.0).** Registry manifest,
capability sidecar, compiled-in defaults (`https://gotify.darkc.sobe.world`,
title `LunarWing`), and the `configure-gotify`/`--gotify-url`/`--gotify-title`
mt-admin paths were re-checked against the current tree; no changes were needed.

## Build and Install

The registry source is `ic/tools-src/gotify`, a symlink to the canonical `lunarwing-gotify-tool/` crate. Prefer the registry workflow from the `ic/` directory:

```bash
cd ic
lunarwing registry install gotify --build
lunarwing tool setup gotify
```

`registry install --build` uses the manifest in `ic/registry/tools/gotify.json`, builds the WASM component from source, and installs its capability sidecar. `tool setup` prompts for the required `gotify_app_token` and stores it through LunarWing's encrypted secrets path.

For manual development builds, run `lunarwing-gotify-tool/build.sh`; it produces `lunarwing-gotify-tool/gotify.wasm`. Install both `gotify.wasm` and `gotify.capabilities.json` below `${LUNARWING_BASE_DIR:-$HOME/.lunarwing}/tools/`, then restart LunarWing.

## Configuration

### Gotify URL and Title

By default the tool sends notifications to `https://gotify.darkc.sobe.world` with the title `LunarWing`. Both are configurable via the workspace config.

**1. Workspace config** — create `config/gotify.json` in the agent's workspace (`~/.lunarwing/workspace/config/gotify.json`):

```json
{"url": "https://your-gotify.example.com", "title": "My Agent"}
```

The tool appends `/message` automatically, so provide just the base URL. The `title` field is optional — if omitted, the compiled-in default (`LunarWing`) is used. The title serves as the default for notifications when the caller doesn't specify one explicitly.

**2. Capabilities file** — edit `gotify.capabilities.json` to allow HTTP to the new host. Update both the `allowlist` host and the `credentials.host_patterns`:

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

Both changes take effect on restart — no WASM rebuild needed.

If `config/gotify.json` is absent or unparseable, the tool falls back to the compiled-in defaults (URL and title).

### Provisioning Support

For a single-instance deployment, use the registry/setup flow above and write the reviewed workspace JSON and capability host allowlist explicitly. The old `scripts/setup-instance.sh` path is deprecated in favor of `lunarwing onboard --quick` and the current extension lifecycle.

For a multi-tenant deployment, MT admin can configure Gotify during provisioning:
```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant myagent --gotify-url https://gotify.example.com --gotify-title "My Agent"
```
Creates the workspace config at provisioning time. The capabilities file is automatically rewritten when WASM tools are installed (`install-wasm`).

To reconfigure an existing tenant:
```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-gotify myagent https://new-gotify.example.com "My Agent"
```

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

Use `lunarwing tool setup gotify` or the authenticated extension setup UI to store the secret. Do not place the token in the workspace JSON or capability file.

## Usage

The agent calls the `gotify` tool with a JSON payload:

```json
{"message": "Deploy complete", "title": "CI", "priority": 8}
```

| Parameter | Type | Required | Default | Description |
|---|---|---|---|---|
| `message` | string | yes | — | Notification body (supports markdown) |
| `title` | string | no | From `config/gotify.json`, or `LunarWing` | Notification title |
| `priority` | integer | no | `3` | 1-3 low, 5-7 medium, 8-10 high |

For routine prompts that use Gotify, see `ic/docs/GOTIFY_ROUTINE_PROMPT.md`.

## How It Works

The tool runs inside the WASM sandbox. It cannot read secrets or environment variables directly. The host runtime:

1. Reads `config/gotify.json` from the workspace when the tool calls `workspace-read`
2. Injects the `gotify_app_token` into the HTTP request's `X-Gotify-Key` header at the host boundary
3. Validates the outbound URL against the capabilities allowlist before sending
