# Multi-tenant external-worker configuration

> **Status: PARTIALLY-FIXED (verified against `51ae5a8` on 2026-07-20).**
> Tenant provisioning now generates Nanocode, Pebble, and OpenCode endpoint
> blocks. In-memory settings merges preserve their bearer tokens, but a later
> full TOML rewrite (for example `/model`) still omits `auth_token` and can make
> the worker return 401 after reload. Provisioning is fixed; persistence is open.

This consolidates `MISSING-CONFIG-FOR-NANOCODE.md` and the current config
round-trip finding.

## 1. Provisioning-time generation

**Status: FIXED.**

Fresh tenants previously lacked `[[sandbox.external_workers]]`, so
`create_job(mode: "nanocode")` had no route until an operator edited
`config.toml`. The current generator is idempotent and writes the per-tenant WSS
URL, timeout, and gateway token:

- `ensure_external_worker_config` (`ic/scripts/lunarwing-mt-admin.sh:2841-2904`)
- `add-tenant` calls it for Nanocode, Pebble, and OpenCode (`:6272-6274`)
- retrofit calls are present in `patch-env` (`:3314-3319,7393-7408`)
- TOML shape is covered by `ic/src/config/sandbox.rs:451-512`

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
(`lunarwing-mt-admin.sh:6246-6249,6569-6584`; parser/build paths at
`:6992-7136`). An endpoint block alone does not build or start a worker.

## 2. Auth-token loss on settings rewrites

**Status: STILL-OPEN (unverified runtime, source-confirmed risk).**

`ExternalWorkerSettings.auth_token` is `Option<SecretString>` with
`#[serde(default, skip_serializing)]` (`ic/src/settings.rs:811-821`). The
settings writer serializes the whole struct (`settings.rs:1237-1260`), and the
`/model` command loads, changes, and saves that struct
(`ic/src/agent/commands.rs:888-903`). Other full-TOML writers include profile
persistence (`ic/src/tools/builtin/memory.rs:420-425`), CLI model changes
(`ic/src/cli/models.rs:121-147,288-313,346-384`), and `config init`
(`ic/src/cli/config.rs:204-231`). A save round-trip therefore omits the worker
token. `ExternalWorkerConfig::resolve_from_settings` then receives the missing
token (`ic/src/config/sandbox.rs:182-197`), so the daemon can connect to the
worker without the bearer expected by `AGENT_AUTH_TOKEN`.

`Settings::merge_from` now restores skipped worker and endpoint tokens directly
from the TOML-parsed overlay after its JSON merge
(`ic/src/settings.rs:1268-1313`). That fixes token loss during an in-memory
configuration merge; it cannot recover a token that an earlier `save_toml`
rewrite already removed from disk.

The existing config tests prove TOML parsing and multi-worker shape, but no test
currently proves that an auth token survives `Settings::save_toml` and a later
load. The post-merge fixup is not a persistence test, so the generator's old
claim that the block "survives" daemon writers remains unsupported.

Potential fixes include a secret-preserving config merge, an env/DB source of
truth for worker tokens, or an explicit post-save re-injection. Do not mark this
closed until a round-trip test and runtime auth check exist.

## Verification record

The provisioning status was verified by current shell call sites and in-tree
config tests. The persistence status was derived from the current serde
attributes, post-merge fixup, and save/load call path; no Cargo command, daemon
restart, or worker connection was run.
