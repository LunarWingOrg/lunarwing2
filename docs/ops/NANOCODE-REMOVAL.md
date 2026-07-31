# Nanocode External Worker Removal

Nanocode is no longer a supported LunarWing external worker (removed in
v2.0.4.0). Supported persistent worker containers are Pebble, OpenCode, and
Kimi Code.

This is a fresh reimplementation (branch `aj/kimi-4-20260731-1803`). The
earlier MR from `remove-nanocode/v2.0.4.0` (and its antecedents
`aj/kimi-4-20260731-0305`, `rmnc/maneint1/v2.0.4.0`) was reviewed as reference
material only; nothing was merged or cherry-picked.

## What was removed

- `lunarcode4lunarwing/` (worker image source) and `nanocode-config/`
  (tool config + vendored `nanocode.nvim`), plus the one-off
  `ic/scripts/start-nanocode-starforce.sh` launcher.
- mt-admin surfaces: `--with-nanocode`, `--nanocode-model`,
  `--nanocode-base-url`, `build-nanocode-worker`, `configure-nanocode`,
  `start_tenant_nanocode`, `NANOCODE_*` env generation, nanocode doctor
  checks, and nanocode columns in `status` / `list` output.
- Onboarding: `lunarwing_mt_onboard` (+ web wizard) worker option, overrides,
  and import plan field; `hermes_kawarimi` `--with-nanocode` plan flag.
- CI: the manual `worker-nanocode-image` Kaniko job in
  `.gitlab/ci/containers.yml`.
- The `tests/` matrix harness `nanocode_worker` service and scenario.
- Health monitoring: `lunarwing-nanocode-*` units are filtered from both
  `health-openrc.sh` and `health-systemd.sh` (discovered AND explicitly
  configured lists), so a leftover failed unit cannot read critical or
  restart-storm self-heal.

## Rejection paths (migration safety)

- mt-admin rejects the retired flags/commands with an explicit message
  naming the removal release (`add-tenant`, `add-tenants`, `build-tenant`,
  `build-all`, `build-nanocode-worker`, `configure-nanocode`,
  `start-worker … nanocode`).
- `lunarwing_mt_onboard` and `hermes_kawarimi` reject saved configs/plans
  that still select nanocode ("no longer supported"); the web wizard rejects
  `workers: ["nanocode"]` via the worker whitelist.
- `import-tenant.sh` rejects `--with-nanocode`, but **accepts legacy bundle
  manifests** carrying `NANOCODE_MODEL`/`NANOCODE_BASE_URL` and silently drops
  them during injection, so pre-removal exports remain importable.

## Preserved (tenant data, legacy upgrade/teardown, stable registry)

- Port-registry fields `nanocode_wss` (base+7) and `nanocode_health`
  (extended+3) are still allocated and migrated (v2→v3, v8) so port layouts
  never shift. A legacy `.workers.nanocode` registry flag is inert.
- The `nanocode-` tenant-name prefix stays reserved (mt-admin, onboard
  config, and the web wizard) so old per-service units remain
  unambiguous.
- Legacy teardown: `stop-tenant`, `remove-tenant`, `start-tenant`, and
  `patch-env` all run `retire_tenant_nanocode`, which stops legacy
  containers across BOTH rootful and rootless Podman stores and both init
  systems (systemd Quadlet units, OpenRC service + babysitter), unregisters
  the units, strips `NANOCODE_*` from `lunarwing.env`, deletes legacy
  `nanocode.env`, and excises the `create_job(mode: "nanocode")` route
  (including nested endpoint tables) from `config.toml` without disturbing
  adjacent workers. Stop failures abort the operation instead of hiding a
  still-running worker.

Removal is non-destructive: `nanocode-workspace` directories, container
images, stopped containers, and historical registry fields remain for manual
backup or cleanup.

## Operator check after upgrading a tenant

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
```

Back up anything needed from `/home/<tenant>/lunarwing/nanocode-workspace/`,
then remove legacy images/stopped containers manually.

## Rollback

Rollback is a git revert of this branch's commits plus a manual
`build-nanocode-worker`-equivalent image rebuild from a pre-removal checkout
of `lunarcode4lunarwing/`. Tenant data was never deleted, so no data restore
is needed; tenants that already retired simply re-select a supported worker
(`add-tenant <name> --with-pebble|--with-opencode|--with-kimi`).

## Prior MR failure analysis

The rejected MR's required-pipeline failures broke down as:

- **Runner/infrastructure**: image pulls failing with docker.io
  unauthenticated rate limits (`toomanyrequests`) — the same class tracked in
  AGENT_GOALS_2.0.4.0 items #12/#14 as transient runner faults, not
  implementation defects.
- **Implementation-class items re-verified locally for this reimplementation**
  (equivalents of the required checks):
  - `shell-syntax`: `bash -n` clean on every touched script;
    `.gitlab/scripts/shell-syntax.sh` equivalent passes.
  - `python-syntax`: `compileall` over `lunarwing_mt_onboard`,
    `lunarwing_mt_onboard_web`, `hermes_kawarimi` passes.
  - `registry-json`: registry JSON fixtures in the updated suites validate.
  - Affected local suites: `ic/scripts/tests/` (add-tenant-env-flags,
    worker-lifecycle, kawarimi-import-flags, study-mcp-worker-mount,
    wasm-upgrade-build, upgrade-clean-tree, kimi, weechat suites) and the
    health-check suites (49 + 53 assertions) are green; Python unittest
    suites for mt_onboard (83), mt_onboard_web (41), hermes_kawarimi (41)
    are green.

No Rust source changes were required: the daemon never hardcoded a nanocode
worker kind (workers are config-driven `[[sandbox.external_workers]]`
entries), so no `cargo check` surface is affected. The `nanocode` strings
remaining in `ic/src` are generic example/fixture names in tests and doc
comments.
