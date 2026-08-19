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

## Phase 2.5 — Direct server smoke test as the tenant user (while build runs)

The daemon's exact launch command for the managed server is
`python3 <home>/study_notes_mcp.py --db <home>/agent-notes.sqlite3 --notes <home>/agent-notes-by-agent`
(no `--allow-write` in read-only mode — `ic/src/tools/mcp/config.rs:975-998`).
That exact invocation was exercised **as `owlet`** against the shared checkout with a
newline-delimited JSON-RPC client (the server's framing, confirmed at
`study_notes_mcp.py:529-533`):

- `initialize` → `serverInfo={'name': 'study-notes', 'version': '2.0.0'}`.
- `tools/list` → **5 read-only tools**: `knowledge_search`, `knowledge_get_topic`,
  `knowledge_list_topics`, `knowledge_list_stale`, `knowledge_stats`.
- `tools/call knowledge_stats` →
  `{"built_at": "2026-08-18T07:52:28Z", "documents": 515, "topics": 416, "sections": 6244,
   "sources": 1043, "tags": 249, "statuses": {"active": 509, "needs-verification": 2,
   "stale": 3, "superseded": 1}, "write_enabled": false, …}`.
- `tools/call knowledge_search("tenant ports registry", limit 3)` → real LunarWing notes
  (e.g. "Vision Enrichment Subsystem > Multi-Tenant Note", infrastructure-health-check
  study notes) — FTS5 search functional.
- **Read-only guard confirmed**: `knowledge_submit_note` / `knowledge_update_status` are
  NOT exposed without `--allow-write`; `"write_enabled": false` in stats. The tenant
  cannot mutate the shared knowledge base.

Note: FTS5 rejects queries containing bare hyphens (`no such column: admin` for
`mt-admin …`) — server-side query-syntax behavior, not a wiring problem; plain word
queries work.

## Phase 3 — Start + live verification

`start-tenant owlet` → EXIT_RC=0. `mt-admin status owlet`:

- `lunarwing-owlet.service: active`, PostgreSQL container running, LunarVision healthy
  (20025/20026), gateway 10020 / http 10021.

### BUG FOUND — MCP protocol version mismatch (live catch)

Tenant daemon journal (`journalctl --user -u lunarwing-owlet.service`):

```
INFO Reconciled managed Study Notes MCP server home=/var/cache/lunarwing/lw2studymcp allow_write=false
WARN Failed to connect to MCP server 'study_notes': External service error: Unsupported MCP protocol version '2025-03-26'
```

Root cause, verified in code on both sides:

- LunarWing's MCP client sends `protocolVersion: "2024-11-05"` and **only accepted**
  `2024-11-05` in the server's initialize response —
  `ic/src/tools/mcp/protocol.rs` (`PROTOCOL_VERSION`, `SUPPORTED_PROTOCOL_VERSIONS`),
  enforced fail-closed in `ic/src/tools/mcp/client.rs` (`initialize`).
- The lw2studymcp server (`study_notes_mcp.py:33`) answers **every** initialize with
  `PROTOCOL_VERSION = "2025-03-26"`, ignoring the requested version (spec-legal:
  a server may respond with another version it supports; the client then decides).
- Result: the managed `study_notes` server reconciled into config but could never
  connect — a fresh tenant had knowledge *wired* but not *reachable*. This is invisible
  to fixture tests (they never run the real handshake); only the live tenant exposed it.

### Fix (this branch)

`2025-03-26` is wire-identical to `2024-11-05` for the tools-only methods LunarWing's
client uses (`initialize`, `notifications/initialized`, `tools/list`, `tools/call`,
`ping`), so accepting it is safe and strictly improves MCP interop:

- `ic/src/tools/mcp/protocol.rs`: new `PROTOCOL_VERSION_2025_03_26`;
  `SUPPORTED_PROTOCOL_VERSIONS = ["2024-11-05", "2025-03-26"]`. The client still
  **requests** `2024-11-05` (lowest common denominator) but accepts either revision.
- `ic/src/tools/mcp/client.rs` tests: the fail-closed rejection test now uses the truly
  unsupported `2025-06-18`; new `test_initialize_accepts_protocol_version_2025_03_26`
  asserts a `2025-03-26` server negotiates successfully (initialize →
  notifications/initialized, no shutdown).

Verified: `cargo test --lib tools::mcp` — **367 passed, 0 failed** (both touched tests
green). Clippy result recorded below.

### Post-fix live verification

The tenant checkout could not `git pull` (origin is the dev repo under
`/home/applejack`, unreadable by the tenant user), so the two-file diff was applied with
`git apply` (tenant is disposable; the branch carries the real commits), then
`build-tenant owlet` (incremental release, 7m07s) and `restart-tenant owlet`.

Daemon journal after restart — **the WARN is gone**:

```
INFO Reconciled managed Study Notes MCP server home=/var/cache/lunarwing/lw2studymcp allow_write=false
INFO Injecting startup MCP clients into extension manager count=1
```

Tenant's own CLI, as `owlet` with the tenant env (PostgreSQL-backed config):

- `lunarwing mcp list` → `● study_notes - python3 [stdio (python3); host]` — managed
  server seeded into the tenant DB at daemon startup, exactly as designed.
- `lunarwing mcp test study_notes` → **✓ Connection successful!** through LunarWing's
  own MCP client (the same code path the agent loop uses), listing exactly the 5
  read-only tools: `knowledge_search`, `knowledge_get_topic`, `knowledge_list_topics`,
  `knowledge_list_stale`, `knowledge_stats`. No write tools — read-only contract holds
  end to end.
- Gateway sanity: `curl http://127.0.0.1:10021/agent/status` →
  `{"success":true,"data":{"running":true,…}}`.

(Full agent chat was not exercised: the test tenant has no LLM API key provisioned, and
the item targets the study-MCP wiring, which is proven at the MCP layer.)

## Phase 4 — Teardown

```
ic/scripts/lunarwing-mt-admin.sh remove-tenant owlet --purge
```

Result: **EXIT_RC=0** — services stopped, systemd units uninstalled, PostgreSQL
container + data volume removed, ports deallocated, OS user and home directory removed.

Post-teardown assertions (all clean):

- `mt-admin list` → 0 references to `owlet` (back to `alpha`, `antelope`, `hermes`).
- `id owlet` → no such user; `/home/owlet` → gone.
- `/etc/lunarwing/ports.json` → 0 references to `owlet`.
- `podman ps -a` → 0 owlet containers.
- `/var/cache/lunarwing/lw2studymcp` **retained, untouched and clean** — correct: it is
  the shared host checkout future tenants sync/reuse, not tenant state.

## Verdict

**PASS** — with one real bug found and fixed along the way:

1. `add-tenant owlet --with-study-mcp` cloned the shared lw2studymcp checkout at current
   upstream HEAD (`1876c42`) and wired the tenant read-only (env, permissions, port
   block, summary `study mcp: configured (latest lw2studymcp checkout)`). The
   "always latest repo when spinning up a new tenant" requirement works.
2. Live start exposed a protocol-version deadlock (client accepted only `2024-11-05`;
   lw2studymcp answers `2025-03-26`) — fixed on this branch by accepting both revisions
   (`ic/src/tools/mcp/protocol.rs`, `ic/src/tools/mcp/client.rs` tests; 367 MCP tests +
   clippy `-D warnings` green).
3. After rebuild/restart, the tenant daemon reconciled and connected the managed
   `study_notes` server; `lunarwing mcp list`/`mcp test study_notes` as the tenant user
   proved the 5 read-only tools end to end through LunarWing's own MCP client; write
   tools stay gated (`write_enabled: false`).
4. Teardown left zero residue; the shared checkout persists for future tenants.

Follow-up worth considering (out of scope here): upstream lw2studymcp should ideally
echo the client's requested protocol version when it supports it (proper MCP
negotiation) instead of always answering `2025-03-26`; LunarWing now tolerates that
behavior either way.
