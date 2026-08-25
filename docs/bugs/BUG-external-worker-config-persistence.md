# Multi-tenant external-worker configuration

> **Status: FIXED in source and unit tests (verified against current source on
> 2026-08-25); live runtime re-check not yet run.** Tenant provisioning
> generates Pebble, OpenCode, and Kimi endpoint blocks, in-memory settings
> merges preserve their bearer tokens, and `Settings::save_toml` now re-injects
> worker and endpoint `auth_token` values into every full TOML rewrite, with
> round-trip tests to prove it. A live daemon restart plus worker auth check
> has not been executed; once that passes, this report can move to `history/`.

This tracks the current config round-trip finding. Historical Nanocode config
coverage moved to `history/MISSING-CONFIG-FOR-NANOCODE.md` after retirement.

Since v2.0.8.0 `ic/scripts/lunarwing-mt-admin.sh` is a thin composition root
that sources the numbered modules under `ic/scripts/mt-admin-lib/`; the
provisioning references below name the owning module.

## 1. Provisioning-time generation

**Status: FIXED.**

Fresh tenants previously lacked `[[sandbox.external_workers]]` routes until an
operator edited `config.toml`. Current generation is idempotent and writes each
selected supported worker's per-tenant WSS URL, timeout, and gateway token:

- `ensure_external_worker_config`
  (`ic/scripts/mt-admin-lib/09-worker-config.sh:213-292`)
- `add-tenant` calls it for Pebble, OpenCode, and Kimi
  (`ic/scripts/mt-admin-lib/04-tenant-lifecycle.sh:513-515`)
- retrofit calls are present in `patch-env`
  (`ic/scripts/mt-admin-lib/06-env-generation.sh:1944-1946`)
- TOML shape is covered by `ic/src/config/sandbox.rs:146-158,168-176`

The generated shape remains:

```toml
[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:<opencode_wss>/ws/agent"
auth_token = "<tenant GATEWAY_AUTH_TOKEN>"
timeout_ms = 3600000
```

Multi-tenant ports come from the registry rather than the single-node 9090
default. The token mirrors `GATEWAY_AUTH_TOKEN` because the worker starts with
that value as `AGENT_AUTH_TOKEN`.

Worker selection is separate from config generation: `--with-*` flags persist
selection into the ports registry (`.tenants[<name>].workers.<worker>`; the
JSON object is built at `ic/scripts/mt-admin-lib/04-tenant-lifecycle.sh:478-480`
and written by `ports_allocate` at
`ic/scripts/mt-admin-lib/03-ports-registry.sh:649-667`) and the start path
gates workers on that selection (`tenant_worker_enabled`,
`ic/scripts/mt-admin-lib/00-common.sh:294-299`, applied in the start loop at
`ic/scripts/mt-admin-lib/04-tenant-lifecycle.sh:899-910`; `--with-*` parsing at
`ic/scripts/lunarwing-mt-admin.sh:498-500`; build selection at
`ic/scripts/mt-admin-lib/05-tenant-build.sh:421-427,484-500`). An endpoint
block alone does not build or start a worker.

## 2. Auth-token loss on settings rewrites

**Status: FIXED in source and unit tests (live runtime re-check not yet run).**

`ExternalWorkerSettings.auth_token` is `Option<SecretString>` with
`#[serde(default, skip_serializing)]` (`ic/src/settings.rs:881-882`). The
settings writer previously serialized the whole struct and dropped the token.
Since commit `6904394` (2026-07-31), `Settings::save_toml` calls
`reinject_external_worker_tokens` before writing (`settings.rs:1342`), which
re-inserts worker-level and endpoint-level `auth_token` values from the
in-memory settings into the serialized TOML tree (`settings.rs:1386-1458`).
Full-TOML writers such as `/model` (`ic/src/agent/commands.rs:935-960`),
profile persistence (`ic/src/tools/builtin/memory.rs:603-611`), CLI model
changes (`ic/src/cli/models.rs:137-147`), and `config init`
(`ic/src/cli/config.rs:219-230`) therefore no longer strip worker tokens, and
`ExternalWorkerConfig::resolve_from_settings` receives the token after reload
(`ic/src/config/sandbox.rs:185-212`).

`Settings::merge_from` also restores skipped worker and endpoint tokens after
its JSON merge, falling back to the base secrets when the overlay omits them
(`ic/src/settings.rs:1466-1523`), so in-memory configuration merges are covered
as well.

Round-trip tests now exist:
`toml_save_preserves_external_worker_auth_tokens`,
`toml_save_preserves_external_worker_endpoint_auth_tokens`, and
`toml_save_keeps_absent_external_worker_tokens_absent`
(`ic/src/settings.rs:1987-2093`), plus merge coverage at `:2095-2167`. That
satisfies the persistence half of this report's original closing criterion.
The other half — a live daemon restart followed by a real worker request that
no longer returns 401 — has not been executed in this documentation pass, so
the report stays in the active list until that runtime check is done.

The implemented fix is the post-save re-injection described above. Do not move
this report to `history/` until the runtime auth check has been run.

## Verification record

The provisioning status was verified by current shell call sites and in-tree
config tests. The persistence status is confirmed by the `save_toml`
re-injection path and its round-trip unit tests; no Cargo command, daemon
restart, or worker connection was run. Re-verified on 2026-08-13 (revision
`7e813db4`) with updated source line references; `lunarwing-mt-admin.sh` grew
substantially since the previous pass (LunarVision/LunarVoice supervision
work), and the worker-selection refs now name the registry-backed
`tenant_worker_enabled` gate explicitly. Re-verified on 2026-08-20 (revision
`2d10832a`): mt-admin was split into `scripts/mt-admin-lib/NN-*.sh` modules in
v2.0.8.0, so all provisioning references were re-mapped to the owning modules
(`00-common.sh`, `03-ports-registry.sh`, `04-tenant-lifecycle.sh`,
`05-tenant-build.sh`, `06-env-generation.sh`, `09-worker-config.sh`); the
`settings.rs` references shifted by a few lines and were refreshed. No status
changed. Re-verified on 2026-08-25 (revision `57e8a36f`): no status changed.
The work-item #65 port-collision work grew `03-ports-registry.sh`
(`ports_warn_block_in_use`, `ports_registry_duplicate_ports`) and shifted two
cited regions — the `patch-env` retrofit calls now sit at
`06-env-generation.sh:1944-1946` and the `add-tenant` `--with-*` parsing arms
at `lunarwing-mt-admin.sh:498-500`; every other reference (including the
`ports_allocate` jq write at `03-ports-registry.sh:649-667` and
`ensure_external_worker_config` at `09-worker-config.sh:213-292`) still matches
exactly. The `settings.rs` re-injection path and its round-trip tests are
unchanged.
