# Item 16 — Study Notes MCP on a Fresh Tenant: Live Test Record

**Date:** 2026-08-19
**Branch:** `aj/kimi-16-20260819-0902`
**Host:** Arch Linux (systemd), 12 cores / 23 GiB RAM, multi-tenant LunarWing host
**Test tenant:** `owlet` (created for this test, purged afterwards)
**Goals item:** `docs/ops/AGENT_GOALS_2.0.8.0.md` #16 — "test study mcp on a fresh tenant -
verify and record all info - then tear down test tenant when done - update existing lunarwing
to have knowledge of itself automatically - https://git.lunarwing.org/cmc/lw2studymcp.git -
if it already exists, update it to use latest repo always when spinning up new tenant"

The implementation half of this item (mt-admin `configure-study-mcp` /
`add-tenant --with-study-mcp`, always-latest shared checkout) landed earlier on this branch
(`rarity/kimi-15-20260818-0901`, cherry-picked from `rarity/kimi-17-20260818-0000`).
This document records the **live** half: a real fresh tenant provisioned with
`--with-study-mcp`, verified end to end, then torn down.

## Pre-flight

- Fixture suite `ic/scripts/tests/test-study-mcp-sync.sh`: **PASS** (clone / ff-only /
  dirty-refusal / non-git-refusal / read-only provisioning / failure containment / CLI pins).
- Upstream reachability: `git ls-remote https://git.lunarwing.org/cmc/lw2studymcp.git HEAD`
  → `1876c42f4397a283e5fda7e9b866fd11c179bac1`.
- `/var/cache/lunarwing/lw2studymcp` did **not** exist before the test (clean-slate clone
  path exercised).
- Existing tenants before test: `alpha`, `antelope`, `hermes` (untouched by this test).

## Phase 1 — Provisioning (`add-tenant owlet --with-study-mcp`)

Command (as root, tmux):

```
ic/scripts/lunarwing-mt-admin.sh add-tenant owlet --with-study-mcp
```

Result: **EXIT_RC=0**. Log evidence (`/tmp/add-owlet.log`):

```
--- Configuring Study Notes MCP (latest lw2studymcp) ---
cloning Study Notes MCP checkout: https://git.lunarwing.org/cmc/lw2studymcp.git -> /var/cache/lunarwing/lw2studymcp
Study Notes MCP configured for tenant 'owlet' from /var/cache/lunarwing/lw2studymcp (write=false)
  study mcp:        configured (latest lw2studymcp checkout)
```

Port block allocated: `10020-10039` (gateway 10020, http 10021, postgres 10023, …).

### Provisioning verification

- Shared checkout cloned at `/var/cache/lunarwing/lw2studymcp`, `git log -1` =
  `1876c42 added more notes` — **byte-identical to upstream HEAD** fetched in pre-flight
  (`1876c42f4397…`). The "always latest repo when spinning up new tenant" requirement is
  satisfied: clone-when-missing produced current HEAD.
- Checkout contains all files the daemon requires
  (`ic/src/tools/mcp/config.rs::study_notes_server_from_home`):
  `study_notes_mcp.py`, `memorydb.py`, `agent-notes.sqlite3`, `agent-notes-by-agent/`.
- Tenant env (`/home/owlet/lunarwing/env/lunarwing.env`, `owlet:owlet` 0600):

  ```
  # Shared LunarWing Study Notes MCP
  LW2STUDYMCP_HOME=/var/cache/lunarwing/lw2studymcp
  ```

  No `LW2STUDYMCP_ALLOW_WRITE` line → **read-only mode** (core writes stay opt-in via
  `configure-study-mcp --allow-write`), matching the provision contract.
- Filesystem access as the tenant user:
  `sudo -u owlet test -r …/agent-notes.sqlite3` → readable;
  `sudo -u owlet test -w …/agent-notes.sqlite3` → **not writable** (root-owned shared
  checkout; read-only for tenants, as designed).

## Phase 2 — Build (`build-tenant owlet`)

```
ic/scripts/lunarwing-mt-admin.sh build-tenant owlet
```

(WASM extension build skipped — the Study Notes MCP is a managed stdio MCP server and does
not need WASM extensions; daemon, gateway, and XMPP bridge are all native.)

_Result: see "Build result" below (filled in during the run)._

## Phase 3 — Start + live verification

_(filled in during the run)_

## Phase 4 — Teardown

_(filled in during the run)_

## Verdict

_(filled in at the end)_
