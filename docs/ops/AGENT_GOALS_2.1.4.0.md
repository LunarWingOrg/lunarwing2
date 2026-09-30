# PRE-RELEASE CHECKLIST for LunarWing v2.1.4.0

## Codename: UNKNOWN

**Open TODOs (v2.1.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [x] Update all external crates that need updating for the entire project. (DONE — v2.1.4.0 crate/dependency sweep, 2026-09-30)
    - **LunarWing-owned packages 2.1.3 → 2.1.4 (11 packages):** daemon `lunarwing`, workspace crates `lunarwing_common`/`lunarwing_safety`/`lunarwing_skills`/`lunarwing_engine`/`lunarwing_health_metrics`, XMPP bridge, and the four WASM channels (darkirc, multica, weechat, xmpp — both the `ic/channels-src/*` entries and their symlinked repo-root homes). All 8 tracked lockfiles refreshed; tracked-manifest/lock audit script confirms **zero project-owned packages or path dependencies remain at 2.1.3** (only external crates rustc-hash/urlencoding/whoami legitimately sit at 2.1.3).
    - **Channel registry bumps (rebuild-only, behavior unchanged):** darkirc 0.1.9→0.1.10, multica 0.1.12→0.1.13, weechat 0.2.10→0.2.11, xmpp 0.1.15→0.1.16. Tool registry versions unchanged (no tool source changes).
    - **External crates:** full `cargo update` (MSRV 1.96.1-respecting, vendoring temporarily disabled for the lockfile pass) across the ic workspace + all 18 satellite lockfile roots under ic/ plus repo-root satellites (pebble4lunarwing, voice-sidecar, ocr-sidecar, weechat_relay, darkirc, unix-socket-client copies, retired gotify reference copies). ~129 crates in the main workspace: clap 4.6.7, reqwest 0.13.5, h2 0.4.19, hyper 1.11.1, hyper-util 0.1.21, uuid 1.26.1, thiserror 2.0.21, russh 0.62.7, rust_decimal 1.43.0, tower-http 0.7.1, flate2 1.1.10, html-to-markdown-rs 3.15.1, syn 3.0.6, zerovec 0.11.8, smallvec 1.16.2, crossbeam 0.8.5 family, quinn 0.11.12, serde_with 3.24.0, pdf-extract 0.12.1, termimad 0.35.5, open 5.4.4; drops the unused rkyv/bytecheck/ptr_meta/rend/kuchikikiki closure.
    - **ic/vendor/ regenerated** via `scripts/refresh-vendor.sh` (cargo vendor --locked --no-delete); 14 stale crate dirs removed after verifying none appear in any synced lockfile; set check clean (1051 dirs match all synced Cargo.lock files).
    - **cargo deny check:** advisories, bans, licenses, sources all ok. The 8 advisory ignores remain valid (deny.toml untouched); fresh no-ignore scan shows only the previously-accepted unmaintained-info class (ttf-parser, rustls-pemfile, heapless/atomic-polyfill, env_logger/atty, fxhash, bincode, h2 0.3.27 legacy libsql chain) — all pre-existing at v2.1.3, none newly introduced by this update.
    - **Worker/CI audit:** kimi-code pin 2.1.1 = current upstream latest (no change); opencode2 stays at v2.0.18 (v2.0.20 update is item 2, deliberately not done here); pebble pin 2d0f3786… = upstream HEAD exactly; opencode v1.18.25 unchanged (deliberately un-adopted); cargo-deny 0.20.2 = latest release; CI rust toolchain pin 1.96.1 unchanged and matches MSRV; no crate-version literals exist in `.gitlab/` CI to sync. Onboard-web demo fake-mt-admin now compiles v2.1.4.
    - **Verification:** cargo check clean on all four feature combos (default --all-targets, postgres, libsql, all-features), wasm32-wasip2 checks green for all four channels + gitlab/ssh tools, xmpp-bridge compiles, `cargo fmt --all -- --check` clean, `cargo clippy --all --benches --tests --examples -- -D warnings` clean, `cargo test --lib` 5215 passed / 0 failed. CI-green-on-scratch-branch not exercised locally (no runner); the equivalent local gates above cover the CI script surface. Commits: 5791e8ae4, 613cdd250, 25bbc390b, cf6382fc5, 67592092a, 77b4b50be, 31b204c7b + this one.
    - Note: `ic/kCargo.toml` is a stale v1.1.3 shadow-manifest accidentally committed in the 2026-08-15 merge; unreferenced by CI/deliverables. Flagged for cleanup; not modified here (it is not a 2.1.3 LunarWing crate — it predates 2.x).
2. [ ] update external workers - opencode2 to 2.0.20 - NEW VERSION - update from 2.0.18

## Phase 2 — Features & Polish

3. [ ] audit all crates and ensure there are no security vulnerabilities

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.2.0.md (candidates, weigh each)

4. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/268
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/265
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/264
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/244
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/243
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
18. [x] (S) perform write up on october experiment
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
