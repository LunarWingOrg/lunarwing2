# PRE-RELEASE CHECKLIST for LunarWing v2.1.2.0

## Codename: UNKNOWN

**Open TODOs (v2.1.2.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [x] Update all external crates that need updating for the entire project. See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.1 to 2.1.2 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.1 crates left. Refer to goals doc from aragents/kimil00p/v2.1.0.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.1 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. See older releases for more info on this if any clarification is needed.
    - DONE repo-side 2026-09-14 (`rarity/kimi-1-20260914-0000`): bumped all 11 LunarWing-owned release packages from 2.1.1 to 2.1.2 (daemon, five workspace crates, XMPP bridge, and four WASM channels), refreshed all six affected lockfiles, and bumped all four channel registry packages (DarkIRC 0.1.8, Multica 0.1.11, WeeChat 0.2.9, XMPP 0.1.14). A tracked-manifest/lock audit confirms no project-owned package or path dependency remains at 2.1.1 (the only `2.1.1` lock entries left are the external `derive_more`/`derive_more-impl` crate pins).
    - External crates: all 26 tracked lockfile roots report `Locking 0 packages` in `cargo update --workspace --dry-run` sweeps. Monty stays at the approved 0.0.17 release commit (per-VM limits, allowlisted Astral Ruff source), wnaf stays at non-yanked 0.14.1, Wasmtime/wasmtime-wasi stay at 36.0.14, and the remaining behind-latest packages are MSRV-1.96.1-blocked majors (aes 0.9, ed25519-dalek 3, hkdf/hmac/sha2 next lines, tokio-tungstenite 0.29, etc.). `ic/scripts/refresh-vendor.sh` regenerated `ic/vendor/` (1,053 dirs, 844 unique names, set matches every synced lockfile) and produced no diff because no external version changed.
    - Worker/CI audit: Kimi Code bumped 0.41.0 → 0.42.0 (npm `latest`; Dockerfile ARG, build-worker-image.sh build-args, kimi-cli package.json + regenerated package-lock, and the mt-admin harness assertion all agree; node 22 base satisfies the new `>=22.19.0` engines). Pebble remains at upstream HEAD (2d0f3786), OpenCode remains deliberately pinned to proven v1.18.25 (v1.18.26+ unproven; v1.18.29 has the documented live regressions). Rust 1.96.1 remains the declared MSRV and CI pin; checksum-pinned cargo-deny 0.20.2 remains current. `validate_config.py` and `version-policy.sh` pass; shell harnesses pass except `test-coord-knowledge-matrix.sh`, which requires root (environmental, not a pin issue). A remote scratch-branch pipeline was not triggered in-session because this task did not authorize a push.
    - Verified clean: `cargo check --locked` for default, PostgreSQL-only, libSQL-only, and all-features; XMPP bridge native check; all four channel crates on `wasm32-wasip2`; `cargo deny check` (advisories, bans, licenses, sources all ok); rustfmt; clippy `-D warnings` on all features.
2. [x] look into https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/139 and address if valid
    - DONE 2026-09-14 (`aj/kimi-2-20260914-1800`): WI#139 "corruption cause thesis" is VALID and was verified at file:line on the current tree. All three relay stages check out: Stage 1 detection in `ic/src/llm/stream_integrity.rs:305-331` (RepeatedPhrase + RoleDelimiter detectors at :378-388, marker + `LlmStreamObservation::Corrupted` queued ahead of Done); Stage 2 payload stripping at `ic/src/bridge/llm_adapter.rs:168-169` (`Corrupted { .. }` → bare `ProviderStreamObservation::Corrupted`, supplier-controlled provider/kind strings discarded at the boundary); Stage 3 enforcement at `ic/crates/lunarwing_engine/src/executor/llm_stream.rs:117-118` (`finish_output` corrupted branch returns `LlmResponse::Text(self.text)`, parsed tool calls thrown away) with the locking regression test `corrupted_output_is_retained_as_non_executable_text` (:249). The WI's stated "honest gap" (orchestrator recovery-side) is also closed: `orchestrator.rs:792-876` renders observations as a single static `context_line()` string per variant (`traits/llm.rs:54-67`) — no provider text crosses into engine logs or error surfaces.
    - One real gap found and fixed: the BLOCKING `complete_with_tools()` path in `StreamIntegrityProvider` only appended the integrity marker but still returned tool calls from corrupted output (unlike the streaming path). Now clears `response.tool_calls` when corruption is detected, so corrupted output can never reach a tool executor. New regression test `corrupted_blocking_tool_response_demotes_calls_to_text`; all 8 stream_integrity tests pass, `cargo check --lib` clean.
3. [x] Durable cross-channel attachments — a file sent on one channel stays retrievable from all other channels
    - DONE 2026-09-14 (`rarity/kimi-3-20260914-0601`): added `crate::agent::attachment_store` — every inbound attachment is persisted into workspace memory under `attachments/<channel>/<id>/<filename>.md` (sanitized metadata, extracted text, bounded base64 inline data) via the normal workspace write path, so it is injection-scanned, search-indexed, and visible in the WebUI memory tree. Hooked into `Agent::prepare_message` so all channels (XMPP, Signal, DarkIRC, WASM channels, WebUI) persist attachments before processing.
    - Added the built-in `attachment_store` tool (list/read) registered alongside memory tools in both single-user and multi-tenant paths, protected from dynamic shadowing, read-only under skill attenuation, seeded AlwaysAllow in permissions, and schema-validated. Paths are confined to `attachments/`; engine runtime state stays inaccessible.
    - Verified: clippy `-D warnings` clean; full lib test suite 5026 passed / 0 failed (incl. 12 new tests across the store and tool); rustfmt clean. Docs updated in `docs/architecture/ENGINE-V2.md` (attachment input + data retention sections).

## Phase 2 — Polish

4. [x] Address the work item #128 — mt-admin restart parity: pebble worker unit stopped but never restarted** `[bug,mt-admin,2.1.0.0]` - NEW since ship. Any tenant bounce leaves the tenant without its Pebble worker. Small, sharp, operator-visible. Good early quick-win.
    - Pebble Quadlet shutdown now stops the owning systemd user unit first, with a container-stop fallback, and startup clears any `StartLimitBurst` lockout before restarting.
    - Regression coverage exercises the stop/start bounce ordering and fallback; `scripts/check-mt-admin.sh` plus `scripts/tests/test-mt-admin-worker-lifecycle.sh` passed.

5. [x] (S) **#122 — New human supervised LunarVision test** - Unaddressed. Human-in-the-loop acceptance testing for LunarVision

6. [ ] **#114 — feature: generic sandbox — podman impl** `[low]` - Never addressed. Sandbox abstraction has docker; podman impl missing - See:  generic-podman-sandbox/concept-branch/v2.1.2.0 for reference

7. [x] (S) **#113 — studynotesfindings — go over all** `[low]` - Never addressed. Sweep/verify all study-notes findings

8. [ ] investigate cargo deny failure in ci/cd pipeline and fix please

9. [x] (S) Mascot Design Art Project

10. [x] (S) Reserved for note for human about checkpoint #3 for manual test tenant box (i dont even remember what this was for)

## Phase 3 — Loose Ends (DONE-trail residuals not covered by open WIs)
11. [x] **WeeChat WASM channel trap on fresh tenants** (goal #43, W1/W2 SKIPPED) - VERIFIED NOT REPRODUCIBLE on the current tree (2026-09-09, item-14 pass). Full live repro attempt on tenant `alpha` (real weechat 4.10 + real ws_adapter longpoll + fresh `dm_policy=open` via BOTH delivery paths: caps-file config edit AND durable `settings.extensions.weechat.setup_fields`, restart, sustained DM traffic incl. unicode/2KB payloads, adapter restart mid-stream, group-buffer path): zero traps across every poll for the whole window. Guest crate tests 63/63 pass; a new integration test (`real_weechat_component_does_not_trap_on_fresh_tenant_longpoll_polls` in `ic/tests/engine_v2_irc_wasm.rs`) pins the fresh-tenant longpoll shape (auto mode, adapter `/api/health` with `event_cursor`, empty buffers, `dm_policy=open`) and passes. Finding: the trap does not reproduce with the goal-43 repro recipe on the 2.1.1 tree; the `dm_policy=open` correlation from goal #43 appears spurious (the guest never reads DM policy on a no-event poll — `do_longpoll`→`refresh_policy_config` only writes the value it read). Also closes the goal #9 residual: durable `dm_policy=open` via the settings store was verified working live (unpaired nick `nick:miniirc:peer43` admitted through `check_sender_allowed`), so the test-only edit gap is confirmed fixed by the CHPAR-005 `dm_policy_explicit` machinery. If the trap resurfaces, the next discriminating evidence to capture is the wasmtime backtrace (log `Polling callback failed` at DEBUG with `WASM_BACKTRACE_DETAILS=1`) plus a fuel/epoch check on the tenant engine — see `ic/src/channels/wasm/wrapper.rs` `map_wasm_error`.

12. [x] (S) **Engine V2 interrupt → mascot stuck in `working` up to 30s** (goal #33 residual)
    `ThreadOutcome::Stopped` maps to the empty-string sentinel by !767 design, so engine-interrupt turns emit no terminal `response` event; mascot falls back to 30s idle timer instead of immediate idle. "Parity with legacy `Interrupted.` emission needs a tracker decision." Small polish, user-visible

13. [x] (S) **WI#107 mascot: final human-visible browser animation check** (goal #45)
    Repo-contract PASS (27/27 emote tests, live public-surface checks on antelope/kestrel healthy). Remaining: the human acceptance step — fresh tenant, reply containing `*nickers softly* *flicks her tail*`, watch it animate. Cheap; could fold into any live session

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.0.0.md (candidates, weigh each)

14. [x] (S) **DarkIRC Stages B–D** — still disabled behind separate acceptance gates (deferred per 2.1 board; do NOT resurrect unasked)

## Phase 5 - Documentation
15. [x] take a look at any documentation that should be updated and update necessary documentation
    - DONE 2026-09-14 (`rarity/kimi-15-20260914-1801`): documentation sweep for 2.1.2.0. Added the missing v2.1.1.0 (Zanshin) row to `docs/releases/README.md` and updated `docs/README.md` so the release index points at v2.1.1.0 as latest (was stale at v2.1.0.0). Updated `docs/ops/WORKER-CONTAINERS.md` Kimi Code pin from 0.41.0 to 0.42.0 to match the item #1 worker bump. Verified no other maintained doc needed changes: architecture/ was handled by item #20, guides/ is item #19, release notes are item #21, and the remaining `0.41.0`/`2.1.1` references live in immutable release notes or correct historical records (PEBBLE-WORKER.md WI#128 note correctly says v2.1.1.0).
16. [x] (S) Reserved for something else
17. [x] (S) Reserved for something else
18. [x] review open work items
    - Live tracker query 2026-09-14 00:03 UTC: 14 open issues (#97, #109, #113, #114, #121, #122, #126, #127, #130, #131, #132, #135, #136, #137). Since the v2.1.1.0 audit, #86/#133/#134 closed and #135/#136/#137 opened.
    - All three new findings are fixed in source at 14ccfdd23: #135/#136 mission-goal index exclusion (subtree + purge sweep + provenance labels, live canary acceptance on tracker); #137 bounded OpenRC writer-quiesce wait + status-classification fix. #114 has open draft MR !855 (podman sandbox).
    - No 2.1.2.0 code blockers among open items; remaining work is tracker housekeeping plus operator/live residuals on #97/#131/#137. Report: docs/ops/v2.1.2.0_OPEN_WORK_ITEMS_STATUS.md.
19. [ ] update all docs in docs/guides
20. [x] update all docs in docs/architecture
    - DONE 2026-09-12 (aj/kimi-20-20260914-0600): all 10 architecture docs revalidated against the 2.1.2.0 tree. SEMANTIC-MEMORY-SEARCH gained the WI#132 `indexed_at` outbox (V27) + repair sweep and the WI#135/WI#136 mission-subtree `missions.json` index exclusion; ENGINE-V2 gained WI#132 authenticity hardening (retrieval/compaction labeling + neutralization) and dropped the dead `executor/context.rs` from its module map; SELF_HEAL_DEPLOYMENT_WIRING gained the 2.1.2.0 `stop_tenant` writer-quiescence notes (WI#133/#134/#135/#137). The remaining docs were date-stamped after confirming no substantive change: SELF_HEAL_COVERAGE_AUDIT (ORDER unchanged at 14 suites), ATOMICBOOL_DEEPER_PROPAGATION (anchors unchanged, defer stands), SSH_AGENT_HARNESS, SSH_DELIVERY_MECHANISMS, XMPP_FILE_TRANSFERS, IRC-SENDER-IDENTITY, WEECHAT-CHANNEL-ARCHITECTURE (registry 0.2.9 bump only).
21. [ ] create draft of release notes - at: `RELEASE-v2.1.2.0.md` at the repo root, modeled on the `docs/releases/RELEASE-v2.1.0.0.md` structure (Overview, Highlights, Compatibility, Changes, Upgrade Notes)

---

