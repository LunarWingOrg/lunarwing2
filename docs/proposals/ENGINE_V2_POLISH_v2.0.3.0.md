# Engine V2 Fixes and Polish for v2.0.3.0

**Created:** 2026-07-26
**Author:** Rarity
**Status:** Implemented — all P1 and P2 items landed on 2026-07-26 (goals items
#9 and #11); P3.1/3.3 done, P3.2/3.4 deferred, P3.5 deferred to v2.0.4.0.
Re-verified against source on 2026-07-29.
**Scope:** Engine V2 — mission lifecycle, gate handling, time budgets, provenance, and memory

---

## Purpose

Item #13 from `AGENT_GOALS_2.0.3.0.md` asks: "go over release notes for 2.0.2.0
and see what else still needs to be added to next release ... you must focus on
enginev2 related fixes/polishing."

This document catalogs every Engine V2 known issue or limitation from
`docs/releases/RELEASE-v2.0.2.0.md`, verifies it against current source, and
proposes concrete fixes scoped for v2.0.3.0. Items are grouped by severity and
implementation effort.

---

## Priority 1 — Security and Correctness (should fix for 2.0.3.0)

### 1.1 Wire provenance into production dispatch — **DONE (2026-07-26, item #9)**

**Release note (Known Issues, Security):**
> Engine provenance policy is not wired into production dispatch. Provenance
> types and `evaluate_with_provenance()` exist, but active execution paths still
> use the non-provenance policy method.

**Verified (2026-07-26):**
- `PolicyEngine::evaluate_with_provenance()` exists at
  `ic/crates/lunarwing_engine/src/capability/policy.rs:126`.
- No production call site in `ic/src/` references `evaluate_with_provenance`.
  Grep returns zero matches in the bridge or router.
- The capability evaluation path still calls the non-provenance `evaluate()`.

**Implemented:**
- The production dispatch path now calls `evaluate_with_provenance()` from the
  engine executors (`ic/crates/lunarwing_engine/src/executor/scripting.rs`,
  `orchestrator.rs`, `structured.rs`), passing the message's `Provenance`.
- The taint-based rules (deny `LlmGenerated` access to financial/destructive
  actions) are connected and active.

### 1.2 Enforce ownership/admin checks on mission mutation tools — **DONE (2026-07-26, item #9)**

**Release note (Known Issues, Security):**
> Mission mutation lacks consistent ownership enforcement. Conversational
> mission pause/resume/delete paths do not reliably verify caller ownership or
> admin status. An arbitrary known mission UUID can be completed by `delete`.

**Verified (2026-07-26):**
- `mission_pause` and `mission_resume` do check ownership at the engine layer
  (`ic/crates/lunarwing_engine/src/runtime/mission.rs:272`, `:304`).
- However, `mission_delete` calls `complete_mission(id)` directly at
  `ic/src/bridge/effect_adapter.rs:419` with **no ownership check** — it skips
  `pause`/`resume`'s engine-layer ownership guard entirely.
- `mission_update` does route through `mgr.update_mission()` which checks
  ownership (`:232`), but the conversational path does not verify admin role for
  shared missions.

**Implemented:**
- Added `MissionManager::delete_mission(id, user_id)` at
  `ic/crates/lunarwing_engine/src/runtime/mission.rs:434` with an ownership
  guard; the conversational path at `ic/src/bridge/effect_adapter.rs:432` now
  routes through it instead of calling `complete_mission` directly.
- Tests for ownership-checked delete added in the same change.

### 1.3 Fix `mission_update` name field mismatch — **DONE (2026-07-26, item #9)**

**Release note (Known Issues, Engine):**
> `mission_update` advertises a `name` field while one parser path reads
> `new_name`; a model call can report success without renaming.

**Verified (2026-07-26):**
- Tool schema at `ic/src/bridge/router.rs:1060` exposes the field as `"name"`.
- Parser at `ic/src/bridge/effect_adapter.rs:230` reads `"new_name"`.
- This is a confirmed mismatch — a model calling `mission_update` with
  `{"name": "new_name_here"}` will silently not rename the mission.

**Implemented:**
- The parser at `ic/src/bridge/effect_adapter.rs:229-230` now accepts `"name"`
  and falls back to `"new_name"` for backward compatibility, preferring `"name"`
  (the schema field).

---

## Priority 2 — Reliability and UX (should fix for 2.0.3.0)

### 2.1 Implement background sweep for expired pending gates — **DONE (2026-07-26, item #11)**

**Release note (Known Issues, Engine):**
> Pending-gate expiry remains hardcoded at 30 minutes and cleanup is
> traffic-driven, not a background sweep. A waiting thread can remain waiting
> until later ingress triggers cleanup.

**Verified (2026-07-26):**
- `GatePipeline` and `PendingGate` exist at `ic/crates/lunarwing_engine/src/gate/`.
- The 30-minute expiry is hardcoded; cleanup happens during lookup/restore, not
  via a background task.
- A thread waiting on an expired gate blocks until ingress triggers the cleanup.

**Implemented:**
- `start_pending_gate_sweep` at `ic/src/bridge/router.rs:4762` periodically
  drains expired pending gates and fails their waiting threads.
- Expiry is configurable via `settings.agent.pending_gate_expiry_secs`
  (default 1800s = 30 minutes), read at engine init with a fallback to
  `DEFAULT_PENDING_GATE_EXPIRY` (`ic/src/bridge/router.rs:128`).

### 2.2 Make `mission_delete` actually delete rather than mark completed — **DONE (2026-07-26, item #11)**

**Release note (Known Issues, Engine):**
> `mission_delete` marks a record `Completed` and removes it from active IDs but
> retains it in persistence/list output while reporting it as deleted.

**Verified (2026-07-26):**
- `complete_mission` at `ic/crates/lunarwing_engine/src/runtime/mission.rs:323`
  sets status to `Completed` and removes from active list.
- There is no `delete_mission` that removes the record from persistence.
- The conversational tool reports `{"status": "deleted"}` at
  `effect_adapter.rs:420` but the mission remains in list output.

**Implemented:**
- Added `Store::delete_mission(id)` which hard-removes the record; the engine's
  `MissionManager::delete_mission` calls it after the ownership check
  (`ic/crates/lunarwing_engine/src/runtime/mission.rs:452`).
- `mission_delete` now hard-deletes with the ownership check from item 1.2.

### 2.3 Wire `event:` and `webhook:` cadence dispatchers — **DONE (2026-07-26, item #11)**

**Release note (Known Issues, Engine):**
> `event:` and `webhook:` cadence forms parse but have no runtime dispatcher and
> never fire through the scheduler.

**Verified (2026-07-26):**
- `MissionCadence::OnEvent` and `MissionCadence::Webhook` are valid enum
  variants (`types/mission.rs:63,68`).
- The cron ticker at `mission.rs:1093` treats them as `should_fire = false`.
- `OnSystemEvent` has a dedicated `fire_on_system_event()` dispatcher, but
  `OnEvent` and `Webhook` do not have equivalent bridge-to-engine routing.
- `Webhook` cadence missions should be fireable through webhook ingress, but no
  bridge path currently routes a webhook payload to a `Webhook`-cadence mission.

**Implemented:**
- Added `MissionManager::fire_webhook_mission(path, secret, payload)`
  (`ic/crates/lunarwing_engine/src/runtime/mission.rs:811`), wired to bridge
  webhook ingress at `ic/src/bridge/router.rs:4081`.
- Engine tests cover the webhook dispatch path (`mission.rs:2699` onward).

### 2.4 Load all owner missions at startup, not just configured owner — **DONE (2026-07-26, item #11)**

**Release note (Known Issues, Engine):**
> Startup currently loads scheduled missions only for the configured owner
> project; persisted non-owner missions can remain inactive after restart.

**Verified (2026-07-26):**
- `bootstrap_project` at `mission.rs:160` loads missions for a single `project_id`.
- Missions belonging to other projects or scopes are not loaded into the active
  index.

**Implemented:**
- Added `MissionManager::bootstrap_all_projects()`
  (`ic/crates/lunarwing_engine/src/runtime/mission.rs:189`) backed by
  `Store::list_all_active_missions_global()`; the bridge calls it at startup
  (`ic/src/bridge/router.rs:1253`), so all active missions across projects are
  loaded, not just the configured owner's.

---

## Priority 3 — Hardening and Polish (nice to have for 2.0.3.0)

### 3.1 Add `memory_search` truncation flag — **DONE (2026-07-26)**

**Release note (Known Issues, Memory):**
> `memory_search` emits a budget-limited ranked prefix but no total-match count
> or explicit truncation flag; callers cannot distinguish a complete search from
> an output-budget truncation.

**Implemented:**
- Added `truncated: bool` and `total_matches: usize` to `build_memory_search_output`
  in `ic/src/tools/builtin/memory.rs`.
- The tool now reports whether results were dropped due to output budget and the
  total number of matches found before truncation.
- Web API `MemorySearchResponse` also updated with `total_matches` and `truncated`.
- **Effort:** Small.

### 3.2 Add per-server MCP lifecycle timeout controls — **DEFERRED**

**Release note (Known Issues, MCP):**
> Per-server startup/request/shutdown timeout controls are not implemented.

**Proposed fix:**
- Add `startup_timeout_secs`, `request_timeout_secs`, `shutdown_timeout_secs` to
  MCP server config.
- **Effort:** Medium.
- **Risk:** Low.
- **Note:** This overlaps with item #9 work. Coordinate with that track.

### 3.3 Improve mission ticker fault isolation — **DONE (2026-07-26)**

**Release note (Known Issues, Engine):**
> One malformed or failed mission can abort later work in the current ticker
> cycle.

**Implemented:**
- Wrapped each mission's `load_mission` and `fire_mission_inner` calls in the
  `tick()` method (`mission.rs`) with individual error boundaries. A failure on
  one mission is logged via `tracing::warn!` and the loop continues to the next
  mission instead of aborting via `?`.
- **Effort:** Small.

### 3.4 Add soft-timeout cleanup for overlapping agent turns — **DEFERRED**

**Release note (Known Issues, Engine):**
> Soft agent timeout suppresses late output but allows the timed-out task to run
> briefly; a later turn can overlap old side effects during that window.

**Proposed fix:**
- Track active timed-out task handles and ensure they are fully cancelled before
  the next turn begins, or add a generation guard so late output is rejected.
- **Effort:** Medium.
- **Risk:** Medium — changes task lifecycle.

### 3.5 Add durable mission notification outbox

**Release note (Known Issues, Engine):**
> Mission notifications and history are best-effort rather than durable outbox
> delivery.

**Proposed fix:**
- Add a lightweight outbox table for mission notifications, with retry on
  delivery failure.
- **Effort:** Large.
- **Risk:** Medium — new DB table.
- **Recommendation:** Defer to v2.0.4.0 unless a concrete delivery failure is
  observed.

---

## Priority 4 — Defer to later releases

These items from the v2.0.2.0 known issues are explicitly out of scope for a
v2.0.3.0 polish cycle:

| Item | Reason for deferral |
|---|---|
| Worker-local MCP execution | Large security surface; track separately |
| Automatic npm/pip MCP install | Security-sensitive; track separately |
| Secret injection through stdio env | Security-sensitive; track separately |
| Broad dependency modernization | Separate crate-audit track (item #8) |
| PostgreSQL vector search indexing (pgvector/ANN) | Infrastructure-scale work |
| DarkIRC secure key exchange | Separate track (item #10) |

---

## Summary

| Priority | Items | Effort | Status (2026-07-29) |
|---|---|---|---|
| P1 (Security) | 1.1, 1.2, 1.3 | Small-Medium | ✅ All done (item #9) |
| P2 (Reliability) | 2.1, 2.2, 2.3, 2.4 | Medium-Large | ✅ All done (item #11) |
| P3 (Polish) | 3.1–3.5 | Small-Large | 3.1 ✓ and 3.3 ✓ done; 3.2, 3.4 deferred; 3.5 deferred to v2.0.4.0 |
| P4 (Defer) | — | — | No |

### Recommended minimal set for 2.0.3.0

1. **1.3** — Fix `mission_update` name/new_name mismatch (trivial, 1-line)
2. **1.2** — Add ownership check to `mission_delete` (small, security)
3. **3.3** — Mission ticker fault isolation (small, reliability)
4. **1.1** — Wire provenance into production dispatch (small wiring change)
5. **2.1** — Background sweep for expired gates (medium, reliability)
6. **2.4** — Load all active missions at startup (medium, correctness)

These six items address the most impactful Engine V2 issues from the v2.0.2.0
release notes with minimal risk and effort.
