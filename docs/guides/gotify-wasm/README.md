# LunarWing Gotify WASM Tool

A WASM tool for LunarWing that sends push notifications to a self-hosted Gotify server.

The registry manifest and WASM source crate are both version `0.1.9`.

## Build and Install

The registry source is `ic/tools-src/gotify`, a symlink to the canonical `lunarwing-gotify-tool/` crate. Prefer the registry workflow from the `ic/` directory:

```bash
cd ic
lunarwing registry install gotify --build
```

`registry install --build` uses the manifest in `ic/registry/tools/gotify.json`, builds the WASM component from source, and installs its capability sidecar. Configure the token separately as described under [Secrets](#secrets).

For manual development builds, run `lunarwing-gotify-tool/build.sh`; it produces `lunarwing-gotify-tool/gotify.wasm`. Install `gotify.wasm` and `gotify.capabilities.json` in the running instance's `<base_dir>/tools/` directory, then restart LunarWing. The bare daemon uses `~/.ironclaw` when neither base-directory environment variable is set.

## Configuration

### Gotify URL and Title

Configure your own Gotify base URL before sending notifications. The guest
falls back to `DEFAULT_GOTIFY_URL` in `lunarwing-gotify-tool/src/lib.rs`
and the title `LunarWing` when it has no usable configuration.

**1. Workspace document** — create `config/gotify.json` in the agent's workspace (the database-backed workspace store, e.g. with the agent's `memory_write`):

```json
{"url": "https://your-gotify.example.com", "title": "My Agent"}
```

**2. On-disk operator config** — for `gotify` and `gotify-tool` with a workspace
read grant covering `config/gotify.json`, the WASM wrapper adds the on-disk
file after successfully listing the workspace. A readable regular file of at most 64 KiB at
`<LUNARWING_BASE_DIR>/workspace/config/gotify.json` takes precedence over the
workspace document. The reader skips a symlink at the file itself. A failed
workspace listing leaves the preload empty. In a managed tenant, mt-admin
writes the file under the tenant state directory.

The tool appends `/message` automatically, so provide just the base URL. The `title` field is optional — if omitted, the compiled-in default (`LunarWing`) is used. The title serves as the default for notifications when the caller doesn't specify one explicitly.

**3. Capabilities file** — update the installed sidecar to allow requests to
the new host. Registry installs use `gotify.capabilities.json`. MT admin
`install-wasm` uses `gotify-tool.capabilities.json`. Update both the `allowlist` host and the `credentials.host_patterns`:

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

Restart after changing the installed capability file to reload the allowlist.
The WASM wrapper reads the workspace config on each execution. In a managed
tenant, use `configure-gotify` to keep the two files in step.

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
Provisioning writes `<tenant state>/workspace/config/gotify.json`. MT admin
`install-wasm` reads the configured URL and updates the installed
`gotify-tool.capabilities.json` allowlist and credential host pattern.

To reconfigure an existing tenant:
```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-gotify myagent https://new-gotify.example.com "My Agent"
```
This writes the on-disk config and updates `gotify-tool.capabilities.json`
if it is already installed. Restart the tenant to load the updated sidecar.

**Test harness** (`lunarwing-xmpp-test-env.sh`):

Set `LUNARWING_TEST_GOTIFY_URL` and optionally `LUNARWING_TEST_GOTIFY_TITLE` before
running `install-wasm`. An unset URL selects the harness script
`ic/scripts/lunarwing-xmpp-test-env.sh`'s built-in endpoint. Set the URL explicitly
to test against your own server.

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

1. Preloads the workspace document and overlays the eligible on-disk operator
   file. The guest looks up `config/gotify.json` with `workspace-read`.
2. Checks the outbound URL against the capability allowlist.
3. Injects `gotify_app_token` as `X-Gotify-Key` on requests to matching hosts.
