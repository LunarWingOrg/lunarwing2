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

Verification: `taskset -c 0-11 cargo test -j12 --lib tools::mcp` — **368 passed,
0 failed** (both touched tests green).

## Live test

### Phase 1 — Provisioning (`add-tenant puffin --with-study-mcp`)

Result: **EXIT_RC=0**. Summary reported
`study mcp: configured (latest lw2studymcp checkout)`; port block `10020-10029`
(gateway 10020, http 10021, postgres 10023, …).

- Shared checkout **fast-forwarded `1876c42` → `278a0bc`** (= upstream HEAD fetched
  in pre-flight, `278a0bc68ce19c…`, `regenned sqlite`). The "always latest repo when
  spinning up a new tenant" requirement is satisfied on the sync path (ff-only pull
  of the retained clean checkout).
- Tenant env (`/home/puffin/lunarwing/env/lunarwing.env`): carries
  `LW2STUDYMCP_HOME=/var/cache/lunarwing/lw2studymcp`; zero
  `LW2STUDYMCP_ALLOW_WRITE` lines → read-only mode; zero legacy `IRONCLAW_` lines
  (item 6 contract also holds on this tenant).
- Filesystem access as the tenant user: `agent-notes.sqlite3` readable, **not
  writable** (root-owned shared checkout) — read-only contract holds.
- Tenant repo clone carries this branch at `96b3aa15` (the protocol-fix commit), so
  the tenant build includes the fix.

### Phase 2 — Build (`build-tenant puffin`)

Running in tmux `build-puffin`, log `/tmp/build-puffin.log`. WASM extension build
skipped — the Study Notes MCP is a managed stdio MCP server; daemon, gateway, and
XMPP bridge are all native. _Result recorded below._

### Phase 2.5 — Direct server smoke test as the tenant user (while build runs)

The daemon's exact launch command (`python3 <home>/study_notes_mcp.py --db
<home>/agent-notes.sqlite3 --notes <home>/agent-notes-by-agent`, no `--allow-write`)
was exercised as `puffin` with a newline-delimited JSON-RPC client:

- `initialize` → `protocolVersion: "2025-03-26"` (the revision the client fix
  accepts), `serverInfo: study-notes 2.0.0`.
- `tools/list` → exactly the 5 read-only tools: `knowledge_search`,
  `knowledge_get_topic`, `knowledge_list_topics`, `knowledge_list_stale`,
  `knowledge_stats`.
- `tools/call knowledge_stats` → 564 documents / 462 topics / 6694 sections,
  `built_at 2026-08-19T23:58:44Z`, `write_enabled: false`.
- `tools/call knowledge_search("tenant ports registry")` → real LunarWing notes
  (ports-registry v16 migration note) — FTS5 search functional.
- Write tools (`knowledge_submit_note`, `knowledge_update_status`) **not exposed**
  without `--allow-write` — the tenant cannot mutate the shared knowledge base.

_RESULTS BELOW — start, MCP verification, teardown._
