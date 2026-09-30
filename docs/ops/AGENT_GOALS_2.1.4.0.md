# PRE-RELEASE CHECKLIST for LunarWing v2.1.4.0

## Codename: UNKNOWN

**Open TODOs (v2.1.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [ ] Update all external crates that need updating for the entire project - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1260 -  See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.3 to 2.1.4 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.3 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.3.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.3 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. there may be docs guidance that you need to update. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
    - IN PROGRESS 2026-09-30 (branch `rarity/kimi-1-20260930-1702`, !1262): the original head `e5128df44` re-landed the !1260 sweep but did not address its three review findings. Authenticated pipeline inspection confirms #4671 and #4672 failed `ci-config` on the unclassified upstream lock; all other MR jobs passed. Pushing the branch alone did not satisfy the CI-green exit criterion.
    - **LunarWing-owned 2.1.3 → 2.1.4 (11 packages):** daemon + 5 workspace crates (`ic/Cargo.toml` + `ic/crates/*`), XMPP bridge (`ic/bridges/xmpp-bridge`), 4 WASM channels (darkirc/multica/weechat/xmpp via their repo-root homes). Channel registry: darkirc 0.1.9→0.1.10, multica 0.1.12→0.1.13, weechat 0.2.10→0.2.11, xmpp 0.1.15→0.1.16.
    - **!1252 P1 #1 fixed:** every `ic/tools-src/*` lockfile updated *together with* its registry bump — cavepony 0.4.2→0.4.3, gitlab 0.1.6→0.1.7, gotify 0.1.0→0.1.1, llm-context 0.1.1→0.1.2, multica-bridge 0.1.2→0.1.3, ssh 0.1.1→0.1.2, vision-analyze 0.1.3→0.1.4, voice-transcribe 0.1.1→0.1.2, web-search 0.2.2→0.2.3.
    - **!1252 P1 #2 / !1254 (pebble rustls RUSTSEC-2026-0285):** pinned upstream `2d0f3786…` ships rustls 0.23.41 with no newer release, so the reviewed patched lock is a hashed build input (`pebble4lunarwing/upstream/Cargo.lock`, only rustls→0.23.45 + rustls-webpki→0.103.15). Dockerfile verifies the pinned `PEBBLE_LOCK_SHA256` digest **and** an allowlist gate on the lock delta; CI `build-worker-image.sh` tracks the lock and checker inputs and inherits the same reviewed digest default as local builds. No unpinned `cargo update`.
    - **External crates:** semver-compatible `cargo update` across the ic workspace (~129 crates: reqwest 0.13.5, h2 0.4.19, hyper 1.11.1, uuid 1.26.1, tower-http 0.7.1, zstd-safe 7.3.0, wasm-bindgen 0.2.129 family, …) plus all 18 satellite lockfile roots under `ic/` and the repo-root satellites (darkirc, weechat, both unix-socket-client copies, both gotify copies, pebble, pebble4lunarwing, ocr/voice sidecars, replv2git copy). 12 held-back deps are all semver-major jumps — deliberately not taken.
    - **Original sweep's reported vendor verification (not rerun for the follow-up):** `refresh-vendor.sh` reported "vendor set matches synced Cargo.lock files", 1148 crate dirs / 833 unique names, `cargo deny check advisories` ok, and zero vendor diff.
    - **Worker/CI audit:** kimi-code 2.1.1 (unchanged), opencode2 v2.0.20 (item 2), opencode v1.18.25, pebble pin `2d0f3786…` = upstream HEAD, rust toolchain 1.96.1 = MSRV. No crate-version literals exist in `.gitlab/` CI to sync. Onboard-web demo fake-mt-admin compiles v2.1.4.
    - **Tracked-manifest/lock audit:** 27 non-vendor lockfiles including the Pebble upstream override; no LunarWing-owned packages or path dependencies remain at 2.1.3 (external rustc-hash/urlencoding/whoami/insta legitimately sit at 2.1.3). Passing standalone metadata for 11 selected units does not prove every tracked lock is classified.
    - **Original sweep's reported local verification (not rerun for the follow-up):** `cargo check --locked` clean on all four feature combos (default `--all-targets`, postgres-only, libsql-only, all-features); `cargo fmt --all -- --check` clean; `cargo clippy --all --benches --tests --examples -D warnings` clean on default **and** `--all-features`; `cargo test --lib` 5216 passed / 0 failed; `cargo deny check` advisories+bans+licenses+sources ok; WASM extension builds green for all tools + channels; `version-policy.sh` (CI blocking job) exits 0; `panic-policy.sh`, `registry_json.py`, `shell-syntax.sh`, `shellcheck.sh`, `python_syntax.py`, `check-lockfile-only-advisories.sh`, `standalone-deliverables.py`, `verify_release_version.py --self-test` all pass. The original run reported 4 shell-harness failures (coord-knowledge-matrix, mt-admin-kimi, ssh-key-upload, tenant-capability-writers) reproducing on the base commit due to dev-VM shell initialization; the remote MR shell-harnesses job passed.
    - **!1262 follow-up:** classify the Pebble lock-only override under `build_inputs` with explicit ci-config and worker-candidate coverage; provide one reviewed Dockerfile digest default for CI/mt-admin/Compose/plain builds; replace the version-line diff guard with parsed complete package-record comparisons. Regression tests reject unrelated additions/removals and source/checksum/dependency-only edits, including a matching-digest override. Python 3.11+ is explicitly installed in the pinned builder for TOML parsing.
    - **Follow-up verification:** the complete ci-config script passes locally (configuration validator, 16 new regression tests, 4 release-version self-tests, promotion self-tests). The real lock from upstream `2d0f3786…` passes the new guard against the committed override. Mocked Docker/Podman slim/fat build entry points pass; both Compose services use the same default. Python/Bash syntax and changed-shell shellcheck pass. This follow-up does not change Rust source, Cargo manifests, lockfiles, or vendor contents.
    - **Still pending:** confirm a new remote pipeline green on the corrected head before checking this item off. Protected worker candidate/clean-rebuild image verification is separate and has not been run for this follow-up; do not infer image-build success from mocked local entry points.
2. [x] update external workers - opencode2 to 2.0.20 - NEW VERSION - update from 2.0.18
    - DONE 2026-09-30 (Applejack, item-2): bumped opencode2 worker v2.0.18 → v2.0.20 at reviewed full commit `84c9be93a56304a108f1a22df0c5d62c26d5b6ca` (tag SHA verified live against the anomalyco/opencode remote).
    - Files: `opencode2.4lunarwing/Dockerfile` (OPENCODE_VERSION/OPENCODE_REF), `.gitlab/scripts/build-worker-image.sh` (CI source_ref + build args), `opencode2.4lunarwing/scripts/prepare_reproducible_build_test.py` (fixture → 2.0.20), `opencode2.4lunarwing/AGENTS.md` + `README.md` (pin refs), `docs/ops/WORKER-CONTAINERS.md` (pin table + bump changelog entry), `docs/guides/CONTRIBUTOR-ONBOARDING.md` (pin mention).
    - Verified: upstream `packages/cli/script/build.ts` at the new SHA still has exactly one `bytecode: true` / `splitting: true` (patch contract holds); bridge surface unchanged (`OpenCode.make`, `session.create` w/ `permissions`, `session.message.list`, `session.permission.reply`, `session.interrupt`, `event.subscribe`, all matched event names; diff is additive — new `credential.list/create` endpoints unused). Host-side `prepare_reproducible_build_test.py` 3/3 OK; shellcheck clean on the CI script. 100% except: whole-image reproducibility must still pass the CI candidate-vs-clean-rebuild digest gate before promotion (CI-only, as with prior bumps).

## Phase 2 — Features & Polish

3. [ ] audit all crates and ensure there are no security vulnerabilities - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1254

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.2.0.md (candidates, weigh each)

4. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/268 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1259
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/265 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1256
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/264 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1255
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/244 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1258
8. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/243
    - DONE 2026-09-30 (`aj/kimi-8-20260930-1200`, item 8): the goal-54 rollover fix was correct on the engine side, but the recurrence root cause sat in the host store. `HybridStore` never overrode `list_all_missions_global`/`list_all_active_missions_global`, so both inherited the trait default that derives from `list_all_projects()`. Missions load by directory scan (`engine/projects/*/missions/*/mission.json`) while projects load from `project.json`, so a mission whose project record was missing/unparseable loaded into the missions map but was invisible to every global listing — never bootstrapped into the active index, so no startup/tick rollover, no event/webhook dispatch, and `threads_today_date` pinned at its creation date (matches the dwarfstar `expected-behavior`/`canary-delta-m4t` evidence). Both globals now enumerate the missions map directly (same semantics the engine test store already had).
    - Verified: new libsql regression `global_mission_listings_see_missions_whose_project_file_is_missing` (missions persisted without their project record remain globally enumerable; only Active ones reach the active-only listing) passes; full engine mission suite and lib clippy with `--all-features -D warnings` pass; `docs/architecture/ENGINE-V2.md` scheduling contract updated. Live dwarfstar acceptance (inspecting persisted `mission.json` after a day rollover) remains an operator step.
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/220
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/191
11. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/184

## Phase 5 - Miscellaneous

12. [ ] take a look at any documentation that should be updated and update necessary documentation
13. [ ] create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for reference)
14. [ ] review all currently open work items - create audit document for 2.1.3.0
15. [x] (S) (COMEBACKTOTHISLATER) UPDATE RELEASE-v2.1.2.0.md WITH CHANGES MADE SINCE FIRST DRAFT WAS CREATED
16. [ ] update all docs in docs/guides
17. [ ] update all docs in docs/architecture
18. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/269
19. [x] (S) compile proper list of changes wanted for web gateway itself
20. [x] fix issues in CI/CD
21. [x] (S) (HUMAN) explore wireguard tun for tunnelling system for podman sandbox
22. [x] (S) (H) Consider web gateway additional functionality (seperate from goal 38)
23. [x] (S) new reconcile method for MR review process
24. [x] (S) record deferred items somewhere in one place
25. [ ] come up with ideas for names for 2.1.4.0
26. [x] (S) (HUMAN) start thinking about scope of 2.1.6.0 next real feature set
27. [x] (S) (HUMAN) Findings from sb and kumo
28. [ ] Repo housecleaning - get rid of old stuff in docs/ops and move it to history

---
