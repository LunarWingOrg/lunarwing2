# MT-Admin Refactor Mapping (v2.0.6.0)

Branch: `sunburst/mtadmin-refactor-v2.0.6.0` (based on `master`)
Date: 2026-08-08
Companion proposal: `docs/proposals/MT-ADMIN-DECOMPOSITION.md` (original, v2.0.3.0 era)

## Goal

The original refactor branch (`mtadminrefactor/meta4/v2.0.3.0`) split
`ic/scripts/lunarwing-mt-admin.sh` (7,748 lines) into 13 library modules
behind a 910-line composition root. Master has since grown to **11,899 lines**
and **355 functions**. This mapping routes the **147 new functions** (added
since the refactor split) into the existing 13-module architecture, and
notes where existing modules need updating.

## Method

- Refactor branch lib functions extracted from `git archive` of
  `remotes/origin/mtadminrefactor/meta4/v2.0.3.0` (`ic/scripts/mt-admin-lib/`).
- Current master function list from `grep '^[a-zA-Z_][a-zA-Z0-9_]*()'` on
  `ic/scripts/lunarwing-mt-admin.sh`.
- Delta = `comm -23 master refactor-lib` = **147 new functions**.

## New-function routing map (147 functions)

### → `00-common.sh` (shared utilities) — 11 functions
```
inherited_lock_fd_is_held        # lock-fd check used by lifecycle locks
_quote_env_value                 # env value quoting helper
_env_set                         # env file KEY=VALUE setter
_read_secret_file                # secret file reading helper
read_tenant_env_value            # tenant env file reader
container_name_list_has          # list containment check
snapshot_sqlite_database         # sqlite snapshot helper
_healthy                         # generic healthy() check
_voice                           # voice container helper
voice_models_ready               # voice readiness check
render_template_content          # template rendering (content variant)
```

### → `detect.sh` (init system / container runtime) — 0 new
(No new detection functions; existing ones unchanged.)

### → `containers.sh` (container lifecycle) — 8 functions
```
_container_config_hash           # changed signature? verify
_render_tenant_units_locked      # locked unit-render wrapper
_stop_nanocode_containers        # nanocode container stop
_stop_nanocode_container_store   # nanocode container-store stop
_stop_nanocode_supervision       # nanocode supervisor stop
_remove_nanocode_supervision     # nanocode supervisor removal
tenant_writer_containers         # writer container list
tenant_writers_probe_status      # writer probe status
```

### → `ports-registry.sh` (port allocation/migration) — 12 functions
```
ports_migrate_v12                # NEW migration (post-refactor)
ports_migrate_v13                # NEW migration
ports_migrate_v14                # NEW migration
ports_migrate_v15                # NEW migration
ports_registry_require_readonly  # read-only registry guard
ports_retire_nanocode            # nanocode port retirement
ports_set_voice                  # voice port assignment
ports_set_weechat                # weechat port assignment
ports_set_worker_desired_running # worker desired-runtime port
ports_enable_worker              # (verify exists in refactor lib)
ports_get                        # (verify exists in refactor lib)
ports_list                       # (verify exists in refactor lib)
```

### → `tenant-build.sh` (build pipeline) — 8 functions
```
build_kimi_worker                # kimi worker image build
build_voice_sidecar_image        # voice sidecar image build  (NEW domain)
ensure_darkirc_build_user        # darkirc build user
ensure_darkirc_release           # darkirc release ensure
run_darkirc_manifest_validator   # darkirc release validation
reject_legacy_darkirc_path_overrides  # path override guard
_run_tenant_build                # core tenant build runner
install_wasm_all / install_wasm_tenant  # wasm install steps
```

### → `env-generation.sh` (env writing) — 10 functions
```
configure_voice                  # voice env/config
configure_study_mcp              # study MCP env
initialize_study_notes_state     # study notes state init
write_tenant_voice_env           # write_tenant_voice_env
write_kimi_runtime_env           # kimi runtime env
patch_tenant_coordination_env    # coordination env patch
patch_weechat_caps               # weechat capabilities patch
tenant_voice_enabled             # voice feature flag
tenant_weechat_enabled           # weechat feature flag
study_notes_write_enabled        # study notes flag helper
```

### → `units-openrc.sh` (OpenRC unit rendering) — 15 functions
```
render_voice_openrc_unit         # voice OpenRC unit
cleanup_legacy_weechat_openrc    # legacy weechat OpenRC cleanup
cleanup_legacy_weechat           # legacy weechat cleanup dispatcher
openrc_service_remove_runlevels  # runlevel cleanup
openrc_worker_stop_disable       # worker stop/disable OpenRC
_nanocode_openrc_service_known   # nanocode service detection
start_tenant_openrc              # OpenRC tenant start
stop_tenant_openrc               # OpenRC tenant stop
restart_weechat_adapter          # weechat restart
set_tenant_weechat_enabled       # weechat enable toggle
_set_tenant_weechat_enabled_locked   # locked variant
weechat_lifecycle_enable_start   # weechat lifecycle
weechat_lifecycle_stop_disable   # weechat lifecycle
weechat_reconcile_units          # weechat unit reconciliation
depend()                         # OpenRC depend stubs (×N, dedupe)
```

### → `units-systemd.sh` (systemd unit rendering) — 5 functions
```
render_voice_quadlet             # voice quadlet rendering
remove_tenant_voice_unit         # voice unit removal
cleanup_legacy_weechat_systemd   # legacy weechat systemd cleanup
start_tenant_systemd             # systemd tenant start
stop_tenant_systemd              # systemd tenant stop
```

### → `worker-config.sh` (external workers) — 10 functions
```
configure_kimi                   # kimi worker config
configure_opencode               # opencode worker config (verify)
configure_pebble                 # pebble worker config (verify)
build_opencode_worker (verify)   # opencode worker build
build_pebble_worker (verify)     # pebble worker build
prepare_worker_lifecycle         # worker lifecycle prep
prepare_worker_workspace         # worker workspace prep
require_worker_name              # worker name validation
start_worker / stop_worker       # worker start/stop
restart_worker                   # worker restart
kill_worker                      # worker kill
remove_external_worker_config    # worker config removal
shared_worker_source_root        # shared source root helper
upgrade_external_worker_timeout  # worker timeout upgrade
```

### → `owner-scope.sh` (owner scope migration) — 8 functions
```
migration_export_boundary        # boundary export
migration_export_boundary_locked # locked variant
migration_export_child           # child export
migration_export_darkirc_locked  # darkirc export
validate_migration_export_boundary      # boundary validation
validate_migration_export_boundary_locked  # locked variant
validate_upgrade_wrapper_boundary       # wrapper validation
validate_upgrade_wrapper_boundary_locked # locked variant
```

### → `postgres.sh` (PostgreSQL lifecycle) — 0 new
(No new postgres functions; existing ones unchanged.)

### → `tenant-lifecycle.sh` (tenant verbs) — 18 functions
```
add_tenant / add_tenants         # add verbs
remove_tenant                    # remove verb
start_tenant / stop_tenant       # start/stop verbs
restart_tenant / _restart_tenant_daemon  # restart verbs
stop_tenant_for_upgrade          # upgrade-stop verb
_upgrade_tenant_lifecycle_locked # upgrade lifecycle locked
upgrade_wrapper_boundary / upgrade_wrapper_boundary_locked  # upgrade wrappers
upgrade_wrapper_child            # upgrade child
upgrade_tenant_darkirc_locked    # darkirc upgrade
retire_tenant_nanocode / _retire_tenant_nanocode  # nanocode retirement
_retire_tenant_nanocode          # (alias)
restore_darkirc_migration_locked # darkirc restore
_archive_nanocode_env            # nanocode env archive
stop_tenant_writers              # writer stop
tenant_writers_active            # writer active check
tenant_writer_services            # writer services list
```

### → `health-pipeline.sh` (health/self-heal) — 4 functions
```
ensure_health_pipeline           # health pipeline ensure
remove_health_pipeline           # health pipeline removal
_install_health_cron             # cron install
_install_health_systemd_timer    # systemd timer install
```

### → `main()` / composition root — 4 functions
```
main                             # new main() dispatch
usage                            # usage text
```

## Changed-function watchlist (verify against refactor lib)

Functions whose *signature or body* may have changed on master (not new,
but potentially different):

- `tenant_darkirc_enabled`, `tenant_proxy_enabled`, `tenant_weechat_enabled`
  → now plain functions; check if logic changed
- `render_worker_openrc_unit`, `render_container_babysitter_unit`
  → unit renderers; verify no new options
- `build_tenant`, `clone_tenant_repo`, `prepare_tenant_checkout`
  → build pipeline may have grown
- `ports_allocate`, `ports_deallocate`, `ports_get`, `ports_list`, `ports_enable_*`
  → check for migration-version bumps
- `render_tenant_systemd_units`, `render_tenant_openrc_units`
  → new unit types (vision, voice) may have been added
- `ensure_worker_config` → may be split into configure_* per worker
- `start_tenant` / `stop_tenant` → may now call voice/vision lifecycle

## Execution order (recommended)

1. **Phase 0 — composition root**: Build `mt-admin-lib/` skeleton + `00-common.sh`
   from current master's utility block. Verify `set -euo pipefail` load order.
2. **Phase 1 — safe modules**: `detect.sh`, `postgres.sh`, `health-pipeline.sh`
   (no cross-deps, low risk).
3. **Phase 2 — medium modules**: `ports-registry.sh`, `owner-scope.sh`
   (complex logic but well-bounded; run the existing
   `test-ports-registry-lib.sh`).
4. **Phase 3 — cross-cutting**: `units-openrc.sh`, `units-systemd.sh`
   (largest blocks; unit renderers must be byte-identical before/after —
   re-run the refactor's unit-comparison verification).
5. **Phase 4 — lifecycle**: `tenant-build.sh`, `env-generation.sh`,
   `worker-config.sh`, `tenant-lifecycle.sh`.
6. **Phase 5 — main()**: thin dispatch + `usage` + `selfcheck`.

## Verification strategy (reuse refactor branch approach)

- `declare -f` byte-identical comparison for all 355 functions before/after.
- Rendered unit comparison: systemd (16 files) + OpenRC (54 files) per tenant.
- CLI output comparison: help, list-tenants, tokens, status, list-backups, doctor.
- End-to-end: provision a fresh tenant through the refactored script
  (add → toolchain → build → start → verify HTTP 200, WASM channels, PG).
- New: `selfcheck` subcommand asserting symbol presence across all 13 libs.
