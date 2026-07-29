# How to Change the Kimi Code Worker Configuration

## Config layers

| File | Role | What you change |
|------|------|-----------------|
| `config/config.toml` | Kimi Code CLI's runtime config (model, provider, thinking) | Default model, providers, thinking settings |
| `entrypoint.sh` | Container startup (config override logic, SSH, bridge launch) | Override behavior, env var handling |
| `scripts/lunarwing_bridge.ts` | The WebSocket bridge (the protocol) | Worker behavior, task dispatch |
| `scripts/kimi_task_executor.ts` | Task execution via Kimi CLI subprocess | Spawn args, stream-json parsing, timeout |
| `health_server.py` | The `/health` + `/ready` endpoints | Health reporting |

## Editing the baked config

```sh
$EDITOR config/config.toml
```

Then rebuild:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker --no-cache
```

## Per-tenant overrides (without rebuild)

Two mechanisms:

1. **Per-tenant LLM model/base-URL** (via `configure-kimi`):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-kimi <tenant> --model <model> --base-url <url>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
```

This sets `KIMI_MODEL` / `KIMI_BASE_URL` env vars in the tenant's `lunarwing.env`. At boot, `entrypoint.sh` materializes an overridden `$KIMI_CODE_HOME/config.toml` from the baked default with these values applied.

2. **Per-tenant API key** (via `configure-kimi --api-key`):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-kimi <tenant> --api-key sk-xxx
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
```

This sets `KIMI_API_KEY` in the tenant's `lunarwing.env`, which `entrypoint.sh` writes into the runtime `config.toml`.

## Two-level config model

1. **Baked defaults** (in `config/config.toml`, applied to *every* tenant at boot unless overridden).
2. **Per-tenant overrides** (via `KIMI_MODEL` / `KIMI_BASE_URL` / `KIMI_API_KEY` env vars, set by `configure-kimi`).

**Priority**: if `KIMI_MODEL`, `KIMI_BASE_URL`, or `KIMI_API_KEY` is set:
→ read baked config, apply overrides, write to `$KIMI_CODE_HOME/config.toml`

else:
→ copy baked config as-is to `$KIMI_CODE_HOME/config.toml`

**Gotcha**: if a tenant has `KIMI_MODEL` set (via `configure-kimi`), editing the baked `config/config.toml`'s `default_model` field **won't affect that tenant** — the override branch wins. That's by design (per-tenant takes precedence).

## Rebuilding

- **Only changed the Rust/TS source that the bridge compiles** → `--no-cache` to be safe, otherwise Docker may reuse cached builder layers.
- **Changed `KIMI_CODE_VERSION`** → `--no-cache` so the new CLI version is installed.
