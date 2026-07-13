# PRE-RELEASE CHECKLIST for LunarWing v2.0.0.0 Codename `Kosoku (光速)`

## Release Codename:  光速
## English Context:   Kōsoku / Kosoku
## Pronunciation:     こうそく (kō-soku)

**Open TODOs (v2.0.0.0) — To be done before release**

---

1. [x] Work on docs/ops/WEECHAT_SERVICES_VERIFICATION.md
2. [x] CREATE DETAILED PLAN OF PHASE 4 OF BIG STREAM Phase 4 is interruption/cancellation; Phase 5 is opt-in WASM delivery.
3. [x] CREATE DETAILED PLAN OF PHASE 5 OF BIG STREAM Phase 4 is interruption/cancellation; Phase 5 is opt-in WASM delivery.
4. [ ] Refine lunarwing web mt admin setup with more user options or build options. We want: GUI splash screen basic black LunarWing text on startup, a toggle for the mascot to enable/disable, toggle for enable/disable show tips on side panel instead of mascot. There is old closed pr with some other stuff w branch rarity/item-11-20260712-1936 not sure if this is even worth checking out or not for this purpose.
5. [ ] PRs and old branches review. Make doc with review.
6. [ ] Inspect status of cargo crates and create documented report of any crates that might still need to be updated. Verify if the info dump below is still correct, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?
    <details>
    <summary><b>INFO DUMP — Crate audit reference</b></summary>
    tower-http | 0.6.10 | 0.7.0 | Available but not required. 0.6.11 patch is available. That's the only crate with a major version available. And it's just 0.7.0 — not a huge jump.
    Things You Might Have Missed:
    1 rand 0.8.6 — Still on 0.8. The dry-run shows 0.10.2 is available but it would be a breaking change (API redesign in 0.9+). If you've deliberately stayed on 0.8, that's fine. Just be aware it's two major versions behind.
    2 base64 0.21.7 — Still on 0.21. Version 0.22 is available. Minor API changes (Engine trait moved). Used in ssh_hostkeys.rs for fingerprint computation.
    3 wasmparser 0.220.1 — Bundled with wasmtime 36, so it's fine. The dry-run doesn't try to update it independently.
    4 pathdiff being removed — The dry-run removes pathdiff v0.2.3 as unused. Good, less deps.
    5 wasip3 being removed — Also cleaned up as unused. Good.
    My Recommendation:
    # Safe to run right now — all patch/minor bumps
    cargo update
    # Then verify
    cargo check --lib
    cargo test --lib
    NOTE: Crates to be deferred until 2.0.0+:
    - rand 0.8 → 0.10 — Breaking, but can be deferred
    - base64 0.21 → 0.22 — Minor breaking, can be deferred
    - tower-http 0.6 → 0.7 — Can be deferred
    This issue documents a crate audit. Current state:
    - tower-http 0.6.10 → 0.7.0: Still on 0.6 in ic/Cargo.toml. Deferred per issue notes.
    - rand 0.8.6 → 0.10.2: Still on 0.8 in ic/Cargo.toml. Breaking change, deferred.
    - base64 0.21.7 → 0.22: Still on 0.21 in ic/Cargo.toml. Minor breaking, deferred.
    - The cargo update (patch/minor bumps) recommendation may or may not have been run.
    Verdict: The deferred crates (rand, base64, tower-http) are intentionally held back for 2.0.0+. The patch-level cargo update should be verified. Genuinely open — deferred to 2.0.0+.
    </details>
7. [ ] Fix any remaining broken cargo tests and ensure updated documentation. Create (or rewrite) new tests if necessary. then re-run cargo tests to ensure
8. [ ] dark irc key exchange. automate the process secruely. For this task I have already prepared a document you can use for implementation: /docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md -  There is also substantial work on this branches already DONE: feat/darkirc-key-exchange-v1 - You can use the following document for reference on next steps: docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md
9. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.0.0 explaining all relevant changes since v1.1.9.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/release for reference as to how to write up this document. The codename for this release is: `Unknown` — The file you write will be RELEASE-v2.0.0.0.md and should be written to the ROOT of the repo.
10. [ ] Improve accuracy of RELEASE-v2.0.0.0.md

---
