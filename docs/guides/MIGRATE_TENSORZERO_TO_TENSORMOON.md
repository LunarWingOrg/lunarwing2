# Migrating LunarWing from TensorZero to TensorMoon

LunarWing used the [TensorZero](https://github.com/tensorzero/tensorzero) gateway as its
model router. TensorZero is no longer maintained, so the organisation forked it as
**TensorMoon** (forked from TensorZero `2026.6.0`; see `FORK.md` in the tensormoon
repository) and LunarWing now targets TensorMoon by default. This guide lists what
changed in LunarWing, what keeps working unchanged, and how to move an existing
deployment over without an outage.

## What changed in LunarWing

| Area | TensorZero (before) | TensorMoon (now) |
| --- | --- | --- |
| OpenAI-compatible model prefix | `tensorzero::function_name::lunarwing` | `tensormoon::function_name::lunarwing` |
| Default `LLM_MODEL` for new tenants (mt-admin, onboarding CLI/GUI, `setup-instance.sh`, `ic/deploy/config.toml`) | `tensorzero::function_name::lunarwing` | `tensormoon::function_name::lunarwing` |
| mt-admin flag / env var | `--tensorzero-url`, `LUNARWING_MT_TENSORZERO_URL` | `--tensormoon-url`, `LUNARWING_MT_TENSORMOON_URL` (old names still accepted, with a warning) |
| Per-tenant `env/proxy.env` | `TENSORZERO_URL` | `TENSORMOON_URL` |
| Worker API key env (OpenCode, OpenCode 2, Kimi) | `TENSORZERO_API_KEY` | `TENSORMOON_API_KEY` (mt-admin passes both for one release; worker entrypoints read either) |
| Gateway config shipped in this repo | `tensorzero-proxy-configurations/tensorzero.toml` | `tensormoon-proxy-configurations/tensormoon.toml` (identical schema) |
| Per-tenant compatibility proxy | `tensorzero-proxy-configurations/lunarwing-proxy.py --tensorzero <url>` | `tensormoon-proxy-configurations/lunarwing-proxy.py --tensormoon <url>` (`--tensorzero` still accepted; the old directory name is a symlink) |
| Hermes Kawarimi importer | `--tensorzero-url`, plan key `tensorzero_url` | `--tensormoon-url`, plan key `tensormoon_url` (old flag and key still read) |
| XMPP test harness | `LUNARWING_TEST_TENSORZERO_URL` | `LUNARWING_TEST_TENSORMOON_URL` (old name still read) |
| Health component / script | `tensorzero`, `health-tensorzero.sh` | `tensormoon`, `health-tensormoon.sh` |
| Health metrics | `lunarwing_health_tensorzero_*` | `lunarwing_health_tensormoon_*` — **breaking for external dashboards and alert rules**; `ic-infrastructure-health-check/contrib/` is updated |
| Health thresholds | `HEALTH_TENSORZERO_*` | `HEALTH_TENSORMOON_*` (old names still read as fallbacks) |
| Self-heal / systemd probe gateway unit | `tensorzero-gateway` | `tensormoon-gateway` (override with `TENSORMOON_SERVICE` and `UNITS`) |
| Rust integration test | `engine_v2_tensorzero_streaming` | `engine_v2_tensormoon_streaming` |
| Gateway deployment itself (outside this repo) | `tensorzero/gateway` image, `tensorzero.toml`, `TENSORZERO_*` env, Postgres schema `tensorzero` | `tensormoon/gateway`, `tensormoon.toml`, `TENSORMOON_*` env, Postgres schema `tensormoon` |

What did **not** change: the OpenAI-compatible surface (`/openai/v1/chat/completions`,
`/openai/v1/models`), `/health` and `/status`, the native `/inference` API, the default
port (3000), and the function names configured in the gateway (`lunarwing`,
`FrontierCODE`, `FrontierCODEK`, ...). LunarWing's Rust LLM adapter is a generic
OpenAI-compatible client and needed no code change.

## Compatibility window

TensorMoon's OpenAI-compatible endpoints accept the legacy
`tensorzero::function_name::`, `tensorzero::model_name::` and
`tensorzero::embedding_model_name::` prefixes as aliases of the `tensormoon::`
prefixes and log a deprecation warning per request. Existing tenants whose
`lunarwing.env` still says `LLM_MODEL=tensorzero::function_name::lunarwing` (and
workers with `OPENCODE_MODEL` / `OPENCODE2_MODEL` / `KIMI_MODEL` set the same way)
therefore keep working the moment the upstream gateway is switched to TensorMoon.

The reverse is **not** true: a TensorZero gateway rejects `tensormoon::` prefixes.
Do not rewrite tenant env files before the gateway cutover.

## Cutover procedure

1. **Deploy TensorMoon on the gateway host.** Build or pull `tensormoon/gateway`,
   use `tensormoon-proxy-configurations/tensormoon.toml` as its config and rename the
   gateway's environment variables (`TENSORZERO_CLICKHOUSE_URL` →
   `TENSORMOON_CLICKHOUSE_URL`, and so on; the provider API keys referenced by
   `api_key_location = "env::..."` keep their names). Rootless podman quadlets ship
   in the tensormoon repository under `quadlets/`. Keep the same host and port so the
   tenants' `LLM_BASE_URL` values stay valid, then verify:

   ```bash
   curl -fsS http://<gateway>:3000/health
   curl -fsS http://<gateway>:3000/openai/v1/models | head -c 300
   ```

   Both prefixes now work against the gateway; existing tenants need no change yet.

2. **Upgrade the admin checkout and the tenants** to a LunarWing release that contains
   this change:

   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh upgrade-tenant <name> --target <ref>
   ```

   `upgrade-tenant` re-renders units, so proxy units reference
   `tensormoon-proxy-configurations/` and pass `--tensormoon`. Units rendered earlier
   keep working through the `tensorzero-proxy-configurations` symlink and the
   `--tensorzero` flag alias; `upgrade-tenant` warns about them until `render-units`
   is rerun.

3. **Rewrite legacy identifiers per tenant** (only once the gateway is TensorMoon):

   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh migrate-tensormoon-env <name> --dry-run
   sudo ic/scripts/lunarwing-mt-admin.sh migrate-tensormoon-env <name>
   sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant <name>
   sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>
   ```

   `migrate-tensormoon-env-all [--dry-run]` covers every registered tenant. The
   command rewrites the `tensorzero::` prefix of `LLM_MODEL`, `OPENCODE_MODEL`,
   `OPENCODE2_MODEL` and `KIMI_MODEL` in `lunarwing.env` (including the
   provider-qualified worker forms `tensorzero/<model>` and
   `tensorzero/tensorzero::function_name::<fn>`) and adds `TENSORMOON_URL` next to
   `TENSORZERO_URL` in `proxy.env`. It is idempotent and touches nothing else.

4. **Operator environment.** Replace `LUNARWING_MT_TENSORZERO_URL` with
   `LUNARWING_MT_TENSORMOON_URL` in shells, wrappers and CI, and `--tensorzero-url`
   with `--tensormoon-url`. The old names keep working but print a warning.

5. **Monitoring.** Update external Prometheus rules and Grafana dashboards from
   `lunarwing_health_tensorzero_*` to `lunarwing_health_tensormoon_*` (updated copies
   live in `ic-infrastructure-health-check/contrib/`). Rename `HEALTH_TENSORZERO_*`
   thresholds in `config.sh` at your convenience. If a host still runs a unit literally
   named `tensorzero-gateway.service`, set `TENSORMOON_SERVICE=tensorzero-gateway` for
   self-heal and include it in `UNITS` for `health-systemd.sh`, or rename the unit.

6. **Workers.** OpenCode and Kimi worker images rebuilt from this release read
   `TENSORMOON_API_KEY` first and fall back to `TENSORZERO_API_KEY`; mt-admin passes
   both names for one release cycle, so an upgraded admin host with not-yet-rebuilt
   worker images stays functional. The baked worker provider is now named
   `tensormoon` (Kimi alias `tensormoon/FrontierCODEK`); entrypoints map overrides
   that still use the old `tensorzero/` provider prefix onto it until step 3
   rewrites them.

## Rollback

Pointing tenants back at a TensorZero gateway is only transparent while their env
files still use `tensorzero::` prefixes, i.e. before step 3. After step 3, roll back by
reverting `LLM_MODEL` (and the worker model keys) by hand, for example with
`configure-opencode <name> --model tensorzero::function_name::...` or by editing
`lunarwing.env`, then restart the tenant.

## Verification checklist

- `curl http://<gateway>:3000/openai/v1/models` from a tenant host lists the configured functions.
- A chat turn succeeds through the daemon, and `health-tensormoon.sh` reports `healthy`
  (or the documented GPU-only fallback when `clickhouse-client` is absent).
- `grep -r 'tensorzero::' /home/<tenant>/lunarwing/env/` is empty after step 3.
- Gateway logs no longer show `Deprecation Warning: The `tensorzero::function_name::` prefix ...`
  once every tenant and worker is migrated.
- `ic/scripts/tests/test-add-tenant-env-flags.sh` and
  `ic-infrastructure-health-check/tests/run-all.sh` pass on the admin host.
