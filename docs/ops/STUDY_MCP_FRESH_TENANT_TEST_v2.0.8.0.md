# Item 16 — Study Notes MCP on a Fresh Tenant: Live Test Record (post mt-admin split redo)

**Date:** 2026-08-20
**Branch:** `aj/kimi-16-20260820-1500`
**Host:** Arch Linux (systemd), multi-tenant LunarWing host
**Test tenant:** `puffin` (created for this test, purged afterwards)
**Goals item:** `docs/ops/AGENT_GOALS_2.0.8.0.md` #16 — "test study mcp on a fresh
tenant - verify and record all info - then tear down test tenant when done - update
existing lunarwing to have knowledge of itself automatically -
https://git.lunarwing.org/cmc/lw2studymcp.git - if it already exists, update it to use
latest repo always when spinning up new tenant".

This is the **redo after the mt-admin refactor** (root script split into
`ic/scripts/mt-admin-lib/NN-*.sh` modules). It re-validates the study-MCP integration
against the post-split layout and re-runs the live fresh-tenant test. The previous
live run (`aj/kimi-16-20260819-0902`, tenant `owlet`) found and fixed a real MCP
protocol-version bug; that fix was **not** carried into the post-split tree and is
re-applied here (see "Protocol fix" below).

## Post-split integration map (verified on this branch)

- CLI surface (root `ic/scripts/lunarwing-mt-admin.sh`): `configure-study-mcp`
  (`--home` optional → shared default checkout + always sync; `--sync` forces
  latest-upstream for an explicit `--home`), `add-tenant`/`add-tenants`
  `--with-study-mcp [--study-mcp-home <path>]`.
- `mt-admin-lib/06-env-generation.sh`: `STUDY_NOTES_REPO` /
  `STUDY_NOTES_DEFAULT_HOME` constants, `sync_study_notes_checkout`
  (flock-serialized clone-when-missing / ff-only pull of a clean checkout, refusing
  dirty or non-git directories), `validate_study_notes_home`.
- `mt-admin-lib/09-worker-config.sh`: `configure_study_mcp`,
  `provision_tenant_study_mcp` (subshell-contained; sync/configure failures stay
  non-fatal to tenant provisioning).
- `mt-admin-lib/04-tenant-lifecycle.sh`: `add_tenant` wiring call
  `provision_tenant_study_mcp "$name" "$study_mcp_home"` with warn + recovery hint,
  add summary reports `study mcp:` status.
- `bash -n` clean over root + all 14 modules.

## Pre-flight

- Fixture suites against the post-split layout (all PASS):
  - `ic/scripts/tests/test-study-mcp-sync.sh` (clone / ff-only / dirty-refusal /
    non-git-refusal / read-only provisioning / failure containment / CLI pins —
    already carries the post-split assertion grepping
    `mt-admin-lib/04-tenant-lifecycle.sh` for the wiring call).
  - `ic/scripts/tests/test-study-mcp-worker-mount.sh`.
  - `ic/scripts/tests/test-add-tenant-env-flags.sh`.
- Upstream reachability: `git ls-remote https://git.lunarwing.org/cmc/lw2studymcp.git
  HEAD` → `278a0bc68ce19c7440243e6b49bda89eb690d5f7`.
- Shared checkout `/var/cache/lunarwing/lw2studymcp` **existed** (retained from the
  prior run, by design), clean, at `1876c42` — one day stale vs upstream, so the
  provisioning sync's ff-only pull to `278a0bc` exercises the "always latest repo"
  requirement.
- Existing tenants before test: `alpha`, `antelope`, `hermes` (untouched by this
  test).

## Protocol fix (re-applied on this branch)

The prior run's live catch: LunarWing's MCP client requested `2024-11-05` and
fail-closed accepted **only** `2024-11-05`, while lw2studymcp's `study_notes` server
answers every initialize with `2025-03-26` (spec-legal) — so a fresh tenant had the
managed server wired but never connectable. The fix commit from
`aj/kimi-16-20260819-0902` was not in the post-split tree; re-applied here
byte-identical (code portions):

- `ic/src/tools/mcp/protocol.rs`: new `PROTOCOL_VERSION_2025_03_26`;
  `SUPPORTED_PROTOCOL_VERSIONS = ["2024-11-05", "2025-03-26"]` (client still
  **requests** `2024-11-05`; `2025-03-26` is wire-identical for the tools-only
  methods used).
- `ic/src/tools/mcp/client.rs` tests: fail-closed rejection test now uses the truly
  unsupported `2025-06-18`; new `test_initialize_accepts_protocol_version_2025_03_26`.

Verification: _RESULTS BELOW_.

## Live test

_RESULTS BELOW — provisioning, build, start, MCP verification, teardown._
