# Multi-tenant external-worker configuration

> **Status: FIXED (2026-07-30, branch rarity/kimi-21-20260730-2101).**
> Tenant provisioning generates Nanocode, Pebble, OpenCode, and Kimi endpoint
> blocks. In-memory settings merges preserve their bearer tokens, and
> `Settings::save_toml` now re-injects the tokens into the on-disk TOML, so
> full-config rewrites (`/model`, profile persistence, CLI model changes,
> `config init`) no longer log the daemon out of its workers. A save/load and
> repeated-rewrite round-trip regression test covers the persistence path.

This consolidates `MISSING-CONFIG-FOR-NANOCODE.md` and the current config
round-trip finding.

## 1. Provisioning-time generation

**Status: FIXED.**

Fresh tenants previously lacked `[[sandbox.external_workers]]`, so
`create_job(mode: "nanocode")` had no route until an operator edited
`config.toml`. The current generator is idempotent and writes the per-tenant WSS
URL, timeout, and gateway token:

- `ensure_external_worker_config` (`ic/scripts/lunarwing-mt-admin.sh:2896-2959`)
- `add-tenant` calls it for Nanocode, Pebble, and OpenCode (`:6356-6358`)
- retrofit calls are present in `patch-env` (`:3372-3374`)
- TOML shape is covered by `ic/src/config/sandbox.rs:155-156,169-179`

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
(`lunarwing-mt-admin.sh:6308,6332,7087-7089`; parser/build paths at
`:7185-7202`). An endpoint block alone does not build or start a worker.

## 2. Auth-token loss on settings rewrites

**Status: FIXED (2026-07-30, branch rarity/kimi-21-20260730-2101).**

`ExternalWorkerSettings.auth_token` remains `Option<SecretString>` with
`#[serde(default, skip_serializing)]` (`ic/src/settings.rs`), so it still
cannot leak through JSON/API transports. `Settings::save_toml` now serializes
through a `toml::Value` and re-injects the worker- and endpoint-level
`auth_token` values into the TOML tree before writing
(`Settings::reinject_external_worker_tokens`). Every full-TOML writer funnels
through `save_toml` — the `/model` command
(`ic/src/agent/commands.rs:885-959`), profile persistence
(`ic/src/tools/builtin/memory.rs:604-611`), CLI model changes
(`ic/src/cli/models.rs:121-143`), and `config init`
(`ic/src/cli/config.rs:204-223`) — and each loads the TOML before saving, so
the in-memory token is rewritten to disk on every save.

The regression test
`settings::tests::toml_round_trip_preserves_external_worker_auth_tokens`
asserts the tokens land in the written file and survive both a save/load
round-trip and a repeated load/mutate/save rewrite (the `/model` pattern).
JSON-side redaction is unchanged: `merge_from`'s post-merge fixup still covers
the in-memory serde_json path.

## Verification record

The provisioning status was verified by current shell call sites and in-tree
config tests. The persistence fix was verified on 2026-07-30 with
`cargo test --lib settings::` (56/56 passing, including the new
`toml_round_trip_preserves_external_worker_auth_tokens`), `cargo clippy --lib
--tests` (zero warnings), and `rustfmt --check` on the touched files. No live
daemon/worker reconnect was run; the round-trip test exercises the exact
save/load path the writers use.
