# PRE-RELEASE CHECKLIST for LunarWing v2.1.5.0

## Codename: UNKNOWN

**Open TODOs (v2.1.5.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [x] Last attempt was merged in but has multiple CI/CD failures.... See pipeline #4679 for all failing jobs... there are multiple.... please address and fix these....Update all external crates that need updating for the entire project - See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.4 to 2.1.5 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.4 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.4.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.4 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. there may be docs guidance that you need to update. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
   - Status: completed 100% (branch `rarity/kimi-1-20261003-0501`, mirroring the v2.1.4.0 item-1 sweep shape).
   - Pipeline #4679's failures were already remediated by v2.1.4.0 goal 1 (docs/ops/v2.1.4.0_GOAL1_CI_RECOVERY.md); master pipeline #4975 is green at merge 0ece377c1, so this item's substantive work was the 2.1.5 dependency sweep below.
   - LunarWing-owned crates 2.1.4 -> 2.1.5 (11 manifests): daemon, lunarwing_common/engine/health_metrics/safety/skills, xmpp-bridge, darkirc/multica/xmpp/weechat-relay WASM channels, and every path-dependency requirement between them.
   - Registry bumps: channels darkirc 0.1.11, multica 0.1.14, weechat 0.2.12, xmpp 0.1.17; tools cavepony-tool 0.4.5 (all five locked surfaces — registry, Cargo package, lock entry, capabilities, CAVEPONY_EXPECTED_VERSION — plus README), gitlab 0.1.8, gotify 0.1.2, llm-context 0.1.3, multica-bridge 0.1.4, ssh 0.1.3, vision-analyze 0.1.5, voice-transcribe 0.1.3, web-search 0.2.4; onboard-web fake mt-admin compile string tracks v2.1.5.
   - External crates (cargo update, MSRV 1.96.1-respecting, 24 swept lockfile roots): cc 1.6.0, html-to-markdown-rs 3.16.0, insta 1.49.0, lazy_static 1.5.1, libc 0.2.190, uuid 1.27.0, wasm-encoder/wasmparser 0.261.0, wast/wat 261.0.0; remaining tool/channel locks were already current.
   - ic/vendor/ regenerated via scripts/refresh-vendor.sh: 1148 crate dirs, vendor set matches synced Cargo.lock files, WI#292 rig-core patch restored and re-verified, advisories/bans/licenses/sources green at vendor time.
   - Local gates: cargo check --locked clean on default/postgres/libsql/all-features; cargo fmt --check clean; clippy --all --benches --tests --examples --all-features -D warnings clean; cargo-deny via .gitlab/scripts/cargo-deny-check.sh clean (yanked gate verifiably enforced, no index-failure); cavepony + xmpp-channel locked release wasm32-wasip2 builds green; ci-config + 41 .gitlab/tests + registry-json + version-policy (vs master base: cavepony/multica/xmpp bumps accepted) + standalone-deliverables (11 units) all pass.
   - Tracked-manifest/lock audit: no project-owned package or path dependency remains at 2.1.4 in any tracked Cargo.toml/Cargo.lock outside ic/vendor/ (only intentional historical fixtures in .gitlab/tests and validate_config.py reference v2.1.4.0 branch names).
   - Worker/CI audit: pebble (upstream commit 2d0f3786 + reviewed RUSTSEC-2026-0285 patched lock), opencode (v1.18.25 hold), opencode2 (v2.0.21), kimi (2.1.1) pins internally consistent across build-worker-image.sh, Dockerfiles, and ci-config cross-checks; infra images digest-pinned (buildkit v0.25.1, trivy 0.74.0, python 3.12-slim). External worker version upgrades left to item 2 per this item's own scope note.
   - CI green on a scratch branch: full-validation pipeline #4987 on rarity/kimi-1-20261003-0501-fullval (LUNARWING_CI_FULL_VALIDATION=1, sha 1136b721ea78) — 26/26 jobs success, including all four check lanes, clippy-all, cargo-deny, lockfile-only-advisory-guard, cavepony-wasm, wasm-channel-test-artifacts, architecture-boundaries, rust-test-lib/libsql/postgres-integration.
2. [x] update external workers - opencode2 to 2.0.21 - NEW VERSION - update from 2.0.20
   - DONE (100%) 2026-10-03: lane was already advanced to v2.0.21 during the v2.1.4.0 cycle (commit `e62ccdee2`, "worker: bump opencode2 v2.0.20 -> v2.0.21"); this pass re-verified every pin and the upstream tag live.
   - Pins confirmed at `v2.0.21` @ `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72`: `opencode2.4lunarwing/Dockerfile` (OPENCODE_VERSION/OPENCODE_REF), `.gitlab/scripts/build-worker-image.sh` (source_ref + build args), `opencode2.4lunarwing/scripts/prepare_reproducible_build_test.py` fixture, `opencode2.4lunarwing/AGENTS.md` + `README.md`, `docs/ops/WORKER-CONTAINERS.md` (v2.0.21 bump entry 2026-10-01), `docs/guides/CONTRIBUTOR-ONBOARDING.md`.
   - Tag re-verified live today: `git ls-remote https://github.com/anomalyco/opencode refs/tags/v2.0.21` → `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72` (matches pin).
   - Host-side `prepare_reproducible_build_test.py` re-run: 3/3 OK, including the untouched-failure contract.
   - No stale `2.0.20` pins remain outside historical docs (history/, release notes, prior bump entries). Whole-image reproducibility still gated by the CI candidate-vs-clean-rebuild check before promotion.

## Phase 2 — Features & Polish

3. [ ] audit all crates and ensure there are no security vulnerabilities

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.3.0.md (for goals referencing work items, read comments on work items as well)

4. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/302
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/301
6. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/300
   - DONE 100% (2026-10-03, branch aj/kimi-6-20261003-0800): watcher's GatePaused arm now discards any pending gate armed for the thread before finalize — new `finalize_regated_expired_turn` helper in `ic/src/bridge/router.rs` (NotFound is the common no-op case). Regression test `expired_gate_regated_wrap_up_discards_rearmed_gate` fixtures a real expiry-resume → re-pause cycle with an armed gate and asserts the store empties (verified red without the discard). Evidence: `cargo test --lib expired_gate` 8/8 pass; `cargo clippy --lib --tests -D warnings` clean; `cargo fmt --all -- --check` clean.
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/299
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/298
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/297
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/296
11. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/296 (dupe)
12. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/295
13. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/294
14. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/291
15. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/290
16. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/289
17. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/288
18. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/287
19. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/278
20. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/276

## Phase 5 - Miscellaneous (for goals referencing work items, read comments on work items as well)

21. [ ] take a look at any documentation that should be updated and update necessary documentation
22. [ ] create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for reference)
23. [ ] review all currently open work items - create audit document for 2.1.5.0 in docs/ops - there are similar docs in docs/ops for 2.1.4.0 and earlier releases so you can use those as references
24. [ ] update all docs in docs/guides
25. [ ] update all docs in docs/architecture
26. [ ] fix issues in CI/CD (if any) - See Pipeline number 4975 for latest master pipeline run and see if anything is not GREEN (cargo deny has issue now at the least)
27. [ ] come up with ideas for names for 2.1.5.0 - write in a document in docs/ops
28. [ ] Repo housecleaning: 1) get rid of old stuff in docs/ops and move it to history 2) remove docs/ov directory
29. [x] (S) RESERVED
30. [x] (S) RESERVED
31. [x] (S) RESERVED
32. [x] (S) RESERVED
33. [x] (S) RESERVED
34. [x] (S) RESERVED
35. [x] (S) RESERVED
36. [x] (S) RESERVED
37. [x] (S) RESERVED
38. [x] (S) RESERVED
39. [x] (S) RESERVED
40. [x] (S) RESERVED

---
