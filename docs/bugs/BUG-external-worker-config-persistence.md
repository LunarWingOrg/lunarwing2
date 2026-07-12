# Multi-tenant external-worker configuration

> **Status: PARTIALLY-FIXED (verified against HEAD 2026-07-12).** Tenant
> provisioning now generates Nanocode, Pebble, and OpenCode endpoint blocks.
> The generated bearer token is marked non-serializable, so a later settings
> rewrite (for example `/model`) can drop `auth_token` and make the worker
> return 401. The original provisioning bug is fixed; persistence is open.

This consolidates `MISSING-CONFIG-FOR-NANOCODE.md` and the current config
round-trip finding.

## 1. Provisioning-time generation

**Status: FIXED.**

Fresh tenants previously lacked `[[sandbox.external_workers]]`, so
`create_job(mode: "nanocode")` had no route until an operator edited
`config.toml`. The current generator is idempotent and writes the per-tenant WSS
URL, timeout, and gateway token:

- `ensure_external_worker_config` (`ic/scripts/lunarwing-mt-admin.sh:2587-2667`)
- `add-tenant` calls it for Nanocode, Pebble, and OpenCode (`:5943-5957`)
- retrofit calls are present in `patch-env` (`:3019-3025`)
- TOML shape is covered by `ic/src/config/sandbox.rs:445-512`

The generated shape remains:

```toml
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:<nanocode_wss>/ws/agent"
auth_token = "<tenant GATEWAY_AUTH_TOKEN>"
timeout_ms = 300000
```

Multi-tenant ports come from the registry rather than the single-node 9090
default. The token mirrors `GATEWAY_AUTH_TOKEN` because the worker starts with
that value as `AGENT_AUTH_TOKEN`.

Worker selection is separate from config generation: `--with-*` flags persist
selection and the start path gates workers on that selection
(`lunarwing-mt-admin.sh:6217-6231` and parser/build paths around `:6640-6765`).
An endpoint block alone does not build or start a worker.

## 2. Auth-token loss on settings rewrites

**Status: STILL-OPEN (unverified runtime, source-confirmed risk).**

`ExternalWorkerSettings.auth_token` is `Option<SecretString>` with
`#[serde(default, skip_serializing)]` (`ic/src/settings.rs:801-813`). The
settings writer serializes the whole struct (`settings.rs:1228-1251`), and the
`/model` command loads, changes, and saves that struct
(`ic/src/agent/commands.rs:884-895`). Other load/update/save callers include
profile persistence (`ic/src/tools/builtin/memory.rs:421-425`) and CLI model or
config writers (`ic/src/cli/models.rs:137-144`, `ic/src/cli/config.rs:218-224`).
A save round-trip therefore omits the
worker token. `ExternalWorkerConfig::resolve_from_settings` then receives the
missing token (`ic/src/config/sandbox.rs:182-195`), so the daemon can connect to
the worker without the bearer expected by `AGENT_AUTH_TOKEN`.

The existing config tests prove TOML parsing and multi-worker shape, but no test
currently proves that an auth token survives `Settings::save_toml` and a later
load. The generator's old claim that the block "survives" daemon writers is
removed from this canonical record.

Potential fixes include a secret-preserving config merge, an env/DB source of
truth for worker tokens, or an explicit post-save re-injection. Do not mark this
closed until a round-trip test and runtime auth check exist.

## Verification record

The provisioning status was verified by current shell call sites and config
tests. The persistence status was derived from the current serde attributes and
save/load call path; no Cargo command, daemon restart, or worker connection was
run.
