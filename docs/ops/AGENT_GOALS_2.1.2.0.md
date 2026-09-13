# PRE-RELEASE CHECKLIST for LunarWing v2.1.2.0

## Codename: UNKNOWN

**Open TODOs (v2.1.1.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [ ] Update all external crates that need updating for the entire project. See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.1 to 2.1.2 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.1 crates left. Refer to goals doc from aragents/kimil00p/v2.1.0.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.1 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. See older releases for more info on this if any clarification is needed.

## Phase 2 — Polish

2. [x] Address the work item #128 — mt-admin restart parity: pebble worker unit stopped but never restarted** `[bug,mt-admin,2.1.0.0]` - NEW since ship. Any tenant bounce leaves the tenant without its Pebble worker. Small, sharp, operator-visible. Good early quick-win.
    - Pebble Quadlet shutdown now stops the owning systemd user unit first, with a container-stop fallback, and startup clears any `StartLimitBurst` lockout before restarting.
    - Regression coverage exercises the stop/start bounce ordering and fallback; `scripts/check-mt-admin.sh` plus `scripts/tests/test-mt-admin-worker-lifecycle.sh` passed.

5. [x] (S) **#122 — New human supervised LunarVision test** - Unaddressed. Human-in-the-loop acceptance testing for LunarVision

7. [x] (S) **#114 — feature: generic sandbox — podman impl** `[low]` - Never addressed. Sandbox abstraction has docker; podman impl missing - ( wait for 2.1.2 )

8. [x] (S) **#113 — studynotesfindings — go over all** `[low]` - Never addressed. Sweep/verify all study-notes findings

12. [x] (S) reserved for CI/CD fixes

14. [x] (S) Mascot Design Art Project

15. [x] (S) Reserved for note for human about checkpoint #3 for manual test tenant box (i dont even remember what this was for)

## Phase 3 — Loose Ends (DONE-trail residuals not covered by open WIs)

16. [x] **WeeChat WASM channel trap on fresh tenants** (goal #43, W1/W2 SKIPPED) - VERIFIED NOT REPRODUCIBLE on the current tree (2026-09-09, item-14 pass). Full live repro attempt on tenant `alpha` (real weechat 4.10 + real ws_adapter longpoll + fresh `dm_policy=open` via BOTH delivery paths: caps-file config edit AND durable `settings.extensions.weechat.setup_fields`, restart, sustained DM traffic incl. unicode/2KB payloads, adapter restart mid-stream, group-buffer path): zero traps across every poll for the whole window. Guest crate tests 63/63 pass; a new integration test (`real_weechat_component_does_not_trap_on_fresh_tenant_longpoll_polls` in `ic/tests/engine_v2_irc_wasm.rs`) pins the fresh-tenant longpoll shape (auto mode, adapter `/api/health` with `event_cursor`, empty buffers, `dm_policy=open`) and passes. Finding: the trap does not reproduce with the goal-43 repro recipe on the 2.1.1 tree; the `dm_policy=open` correlation from goal #43 appears spurious (the guest never reads DM policy on a no-event poll — `do_longpoll`→`refresh_policy_config` only writes the value it read). Also closes the goal #9 residual: durable `dm_policy=open` via the settings store was verified working live (unpaired nick `nick:miniirc:peer43` admitted through `check_sender_allowed`), so the test-only edit gap is confirmed fixed by the CHPAR-005 `dm_policy_explicit` machinery. If the trap resurfaces, the next discriminating evidence to capture is the wasmtime backtrace (log `Polling callback failed` at DEBUG with `WASM_BACKTRACE_DETAILS=1`) plus a fuel/epoch check on the tenant engine — see `ic/src/channels/wasm/wrapper.rs` `map_wasm_error`.

17. [x] (S) **Engine V2 interrupt → mascot stuck in `working` up to 30s** (goal #33 residual)
    `ThreadOutcome::Stopped` maps to the empty-string sentinel by !767 design, so engine-interrupt turns emit no terminal `response` event; mascot falls back to 30s idle timer instead of immediate idle. "Parity with legacy `Interrupted.` emission needs a tracker decision." Small polish, user-visible.

18. [x] (S) **WI#107 mascot: final human-visible browser animation check** (goal #45)
    Repo-contract PASS (27/27 emote tests, live public-surface checks on antelope/kestrel healthy). Remaining: the human acceptance step — fresh tenant, reply containing `*nickers softly* *flicks her tail*`, watch it animate. Cheap; could fold into any live session.

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.0.0.md (candidates, weigh each)

26. [x] (S) **DarkIRC Stages B–D** — still disabled behind separate acceptance gates (deferred per 2.1 board; do NOT resurrect unasked)

## Phase 5 - Documentation
28. [x] take a look at any documentation that should be updated and update necessary documentation
32. [x] (S) Reserved for something else
33. [x] (S) Reserved for something else
35. [x] review open work items
37. [x] update all docs in docs/guides
38. [x] update all docs in docs/architecture
39. [ ] create draft of release notes - at: `RELEASE-v2.1.2.0.md` at the repo root, modeled on the `docs/releases/RELEASE-v2.1.0.0.md` structure (Overview, Highlights, Compatibility, Changes, Upgrade Notes)

---
