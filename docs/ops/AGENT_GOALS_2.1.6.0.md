# PRE-RELEASE CHECKLIST for LunarWing v2.1.6.0

## Codename: Tsuno (角) - "Horns"

**Open TODOs (v2.1.6.0) — To be done before release**

~~Old parity~~
~~Rares = odd (1, 3, 5, 7, ...)~~
~~AJ = even (2, 4, 6, 8, ...)~~
### mod-3 lanes (2026-10-04): Rares = 1 mod 3, AJ = 2 mod 3, Sweetiebot = 0 mod 3


#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [x] Update all external crates that need updating for the entire project - See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.5 to 2.1.6 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.5 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.4.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.4 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. there may be docs guidance that you need to update. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
    - DONE 2026-10-10 (Rarity, item-1, branch `rarity/kimi-1-20261010-0601`): full sweep mirroring the v2.1.5.0/v2.1.4.0 item-1 shape, verified locally end-to-end.
    - **LunarWing-owned 2.1.5 → 2.1.6 (11 packages):** daemon + 5 workspace crates (`ic/Cargo.toml` + `ic/crates/*`), XMPP bridge (`ic/bridges/xmpp-bridge`), 4 WASM channels (darkirc/multica/weechat/xmpp via their repo-root homes; `ic/channels-src/{darkirc,weechat}` are symlinks) plus every inter-crate path-dependency requirement. Channel registry: darkirc 0.1.11→0.1.12, multica 0.1.14→0.1.15, weechat 0.2.12→0.2.13, xmpp 0.1.17→0.1.18. Tool registry: cavepony-tool 0.4.5→0.4.6 (all five locked surfaces: registry, Cargo package, lock entry, capabilities, CI `CAVEPONY_EXPECTED_VERSION`), gitlab 0.1.8→0.1.9, gotify 0.1.2→0.1.3, llm-context 0.1.3→0.1.4, multica-bridge 0.1.4→0.1.5, ssh 0.1.3→0.1.4, vision-analyze 0.1.5→0.1.6, voice-transcribe 0.1.3→0.1.4, web-search 0.2.4→0.2.5. Onboard-web fake mt-admin compiles v2.1.6; cavepony README records the 0.4.6 refresh.
    - **External crates:** semver-compatible `cargo update` (resolver v3 MSRV-fallback, rust 1.96.1) across 26 swept lock roots — ic workspace + xmpp-bridge: arbitrary 1.5.0, async-recursion 1.2.0, cc 1.7.0, either 1.19.0, h2 0.4.20, html-to-markdown-rs 3.17.2, hyper 1.12.0, jiff 0.2.38, mio 1.2.4, smallvec 1.16.3, syn 3.0.7, tokio 1.53.2, tokio-util 0.7.20, toml 1.1.8, uuid 1.28.0, zerocopy 0.8.62, zeroize 1.9.1, zstd-sys 2.1.1, among others; satellites pick up the same patch levels; pebble4lunarwing/voice-sidecar drop the now-unused futures-channel. `pebble4lunarwing/upstream/Cargo.lock` (hashed `PEBBLE_LOCK_SHA256` build input) deliberately untouched. All 26 roots resolve clean under `cargo metadata --locked`.
    - **ic/vendor/ regenerated** via `ic/scripts/refresh-vendor.sh`: 1148 crate dirs / 833 unique names, vendor set matches synced Cargo.lock pins, rig-core WI#292 patch restored and marker-verified (still 0.40.0).
    - **cargo deny:** `advisories ok, bans ok, licenses ok, sources ok` on the `--workspace --all-features` graph (advisory-db `7eebec69`, 2026-10-09); satellite advisory sweep 26 roots ok (pebble upstream manifest-less, gated by verify_upstream_lock.py); lockfile-only advisory guard ok.
    - **Worker/CI audit:** kimi-code 2.1.1 (Dockerfile ARG == kimi-cli package.json/lock pin), opencode v1.18.25+cb7d8b2f, opencode2 v2.0.24 (WI#357-gated, not updated per item 2), pebble ref `2d0f3786…` + lock digest unchanged; rust toolchain 1.96.1 = MSRV; cargo-deny 0.20.2 = latest release; shellcheck 0.11.0 = latest; sccache 0.17.0 (0.18.0 exists; CI cache tool with no crate coupling, deferred). External worker upgrades deferred to a separate goal item per scope note. Only crate-version literal in CI was `CAVEPONY_EXPECTED_VERSION`, bumped with the other cavepony surfaces.
    - **Tracked-manifest/lock audit:** zero LunarWing-owned packages or path dependencies remain at 2.1.4 or 2.1.5 in any tracked Cargo.toml/Cargo.lock outside `ic/vendor/` (only historical doc prose and the historical v2.1.5.0 audit doc mention them).
    - **Verification:** `cargo check --locked` clean on all four feature combos (default `--all-targets`, postgres-only, libsql-only, all-features); `cargo fmt --all -- --check` clean; `cargo clippy --locked -D warnings` clean on default and `--all-features`; all 13 WASM extensions (9 tools + 4 channels) build green via `build-wasm-extensions.sh --strict`; `version-policy.sh`, `standalone-deliverables.py` (11 units), `verify_release_version.py --self-test`, `shell-syntax.sh` (240 scripts), `shellcheck.sh`, `python_syntax.py` (120 files), `panic-policy.sh`, `registry_json.py` (14 files), and the ci-config suite (`test_goal1_ci.py`, 15 tests incl. cavepony drift gate) all pass. Channel-guide version-move notes updated (multica/darkirc/weechat).
    - **Remaining CI-only step:** the branch-push pipeline for `rarity/kimi-1-20261010-0601` on origin must be confirmed green; every locally executable CI job surface above ran green on this exact tree.
2. [x] Due to MULTIPLE various security concerns, including ethics of non-self-hosted relays being added into the project, opencode 2 external worker WILL NOT BE UPDATED - A WORK ITEM HAS BEEN CREATED PROVIDING FULL EXPLANATION AS TO WHY THIS IS BEING GATED - WE WILL REVISIT THIS CONCERN AFTER THE RELEASE OF 2.1.5.0 - CHECKING THIS BOX OFF SO AGENTS STOP ATTEMPTING THIS - Update external workers - opencode2 to 2.0.26 - NEW VERSION - update from current version - refer to previous commits related to opencode2 since the lunarwing 2.1.4.0 release for details - last MR was https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1490 - see work item https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/357

## Phase 2 — Features & Polish (for goals referencing work items, read comments on work items as well)

3. [x] audit all crates and ensure there are no security vulnerabilities - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1505
    - DONE 2026-10-10 (Rarity, item-3, branch `sweetiebot/kimi-3-20261010-1807`) as **risk-acceptance audit, not a clean bill** — per the MR !1505 review (CP-1) this item must not read as "zero vulnerabilities": 6 vulnerability advisories + 1 unsound + 2 unmaintained ship accepted via `ic/deny.toml`, every one accounted for with clearing owner **WI#331** (open, goals item 11). No committed lockfile contains any *other* crate with an active applicable advisory.
    - Re-ran all three committed gates on the post-!1507 tree (audited SHA `a05bd7ac8`, named in the doc): `cargo-deny-check.sh` → advisories/bans/licenses/sources ok; satellite sweep → 26 roots ok + 1 manifest-less skipped; lockfile-only guard → ok. Advisory-db `7eebec69c` (2026-10-09) == upstream HEAD at audit time.
    - New audit doc `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md` addresses every MR !1505 finding (CP-1..CP-8, disposition table at the bottom): honest risk-acceptance conclusion; WI#331 named owner throughout with #304/#305/#306 marked closed/historical; RUSTSEC-2023-0071 exposure rewritten from a fresh code trace (ssh-tool label check dead/bypassable; live path is the auto-confirming `0o666` tenant SSH agent mounted into untrusted opencode/opencode2/pebble workers); advisory-db delta computed by revision range `ef6173c..7eebec69c` (6 real additions, zero lock hits); container-surface claim dropped and the non-expiring `expiring:` trivy suppressions disclosed (filed separately per reviewer); counts corrected (23 locks fully clean, 25 vulnerability-free); no file moves, so no anchor drift.
    - `ic/deny.toml`: every reason string re-verified 2026-10-10, names WI#331, corrected rsa exposure (rc.19 published but not marked patched; russh 0.64.1 still pins `=0.10.0-rc.18`), h2 patched line now 0.4.20. Gate re-run green after the edit.
    - Empty-ignore control run: `ic` flags exactly the 9 accepted IDs, `xmpp-bridge` the 6 webpki/h2/rsa ones — nothing unaccounted. Exposure re-verified with `cargo tree --locked` across feature sets (postgres-only lacks webpki 0.102.8/h2 0.3.27, still has rsa+failure; rental only in all-features).
    - Pointers updated: `.gitlab/README.md:281`, `docs/ops/README.md`. No files moved; v2.1.5.0 audit stays in place.
    - Note: `.gitlab/tests` unittest discovery shows 17 pre-existing failures (test_cargo_deny_gate / test_lockfile_only_advisory_guard / test_rust_bootstrap shell harnesses) on this dev VM — reproduced identically on untouched base `a05bd7ac8`; unrelated to this item (docs + deny.toml text only).
    - Clearing the accepted advisories themselves is **not** part of this item: owned by WI#331 (goals item 11, still `[ ]`)
4. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/298 - Brightdawn confirmed close now
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/324 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1521
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1522
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/326 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1524
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/328 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1525
9. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/329 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1512
    - DONE 2026-10-11 (item-9, branch `sweetiebot/kimi-9-20261011-0005`), re-attempt of the closed MR !1512 (review run-58 verdict "APPROVE WITH NITS"; all findings CY-1..CY-6 addressed in-tree).
    - The six !1512 implementation commits are re-applied (now authored under the lane identity — CY-6): WI#329 fix item (2) single committed list `opencode2.4lunarwing/scripts/sdk_surface.ts` driving both gates, fix item (3) dated WORKER-CONTAINERS corrections (v2.0.11/v2.0.14/v2.0.20) plus AGENTS.md key-file entry, the Dockerfile RUN of `sdk_surface_assert.ts` in place of the inline `bun -e` check, and the self-test mock-parity block. WI#329 fix item (1) was already on the target branch (f3fdc0437/28b75debe): the executor calls top-level `sdk.message.list` with `.data` unwrap and server-contract paging.
    - **CY-1 (Medium, required before WI#329 close):** the self-test now extracts the executor's real `sdk.*(...)` call sites from source (any depth, multi-line aware) and asserts set equality with `BRIDGE_SDK_METHODS`; the reviewer's mutation (a swallowed `sdk.session.summarize` call) now fails the suite, and the overclaiming parity comment was rewritten to describe the actual three-way binding (list ⇔ mock ⇔ executor source ⇔ pinned client via the build gate).
    - **CY-2 (Low):** `BRIDGE_SDK_EVENTS` added (8 `event.type` literals the bridge matches); the self-test binds it to the executor source, and `sdk_surface_assert.ts` additionally asserts each name is still a literal in the pinned `@opencode` client/schema/protocol source shipped in the image (dirs derived from the resolved promise entry; `SDK_SURFACE_SOURCE_DIRS` override for host runs), so an upstream event rename fails the image build instead of hanging tasks at runtime.
    - **CY-3 (Low):** `mockMethodPaths` flattens recursively at any nesting depth.
    - **CY-4 (Info):** goals/provenance wording corrected to "recorded" — `promote-worker-image.py` only checks the manifest has hash lines and a matching `source_sha`; the opencode2 `input_files` manifest now also records the build-executed `opencode_task_executor.ts` and `opencode_task_executor_test.ts` alongside the previously listed inputs (hashed into `worker-inputs.sha256`, recorded at promotion).
    - **CY-5 (Info):** dated WI#329 corrections added to the v2.0.8 and v2.0.15 WORKER-CONTAINERS bump entries (the remaining two that vouched for the broken surface); the v2.0.23 entry's WI#329 follow-up records these !1512 fixes.
    - Verification (host-side, bun 1.3.1): all 4 TS self-tests pass (`opencode_task_executor_test.ts` incl. the new call-site/event bindings, path_expand, opencode_result, task_prompt); `sdk_surface_assert.ts` passes against the REAL pinned v2.0.24 client source (sparse clone at OPENCODE_REF `e7a34f09bfd9134dfade5a8ddb843f7030bc9a69`: 6 methods present, 2 stale absent, 8 event literals present) and fails closed on a missing method path, an event literal renamed in all shipped source copies, and an unreadable source dir; CY-1/CY-2 extraction mutations both caught; `prepare_reproducible_build_test.py` and `study_notes_config_test.py` OK; `bash -n` clean; `.gitlab/tests/test_goal1_ci.py` 15/15 OK. The whole-image candidate build remains the protected-pipeline gate (no MR-pipeline job runs bun or the worker image build), unchanged from !1512.
    - Note: opencode2 stays pinned at v2.0.24 in this lane — the v2.0.26 bump is gated off per checklist item #2 (WI#357) and was not pulled in.
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/330 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1511
11. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/331 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1513
12. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/332

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.3.0.md (for goals referencing work items, read comments on work items as well)

13. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/334 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1514
14. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/337 - last attempt rejected (see comments in closed MR and address if necessary) https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1515
15. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/338
16. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/339
17. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/341
18. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/351
19. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/352
20. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/356
21. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/357
22. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/359
23. [x] work on x
24. [x] (S) (H MR) come back to item 2 at some point after manual review is completed (NOTE FOR LATER)
25. [x] (S) (H MR) release alpha of droid and desk repo projs
26. [x] (S) RESERVED
27. [x] (S) RESERVED
28. [ ] fix issues in CI/CD (if any) - See Pipeline number 5450 for latest master pipeline run and see if anything is not GREEN
29. [x] (S) (H) MASCOT ANIMS
30. [x] (S) RESERVED
31. [x] (S) RESERVED
32. [x] (S) RESERVED
33. [x] (S) RESERVED
34. [x] (S) RESERVED
35. [x] (S) RESERVED
36. [x] (S) RESERVED
37. [x] (S) RESERVED
38. [x] (S) RESERVED

## Phase 5 - Documentation and Misc goal items (for goals referencing work items, read comments on work items as well)

39. [ ] update all documentation under docs/architecture
40. [ ] Create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for references)

---
