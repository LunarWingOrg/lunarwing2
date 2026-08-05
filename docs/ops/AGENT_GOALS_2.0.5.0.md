# PRE-RELEASE CHECKLIST for LunarWing v2.0.5.0

## Codename: Undecided

**Open TODOs (v2.0.5.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] update all external crates that need updating for entire project. see previous release notes for some examples of such crates - ensure CI/CD is also updated for the new crate versions — DONE (2026-08-05): `cargo update` applied to all 18 tracked lockfiles (ic/ workspace + satellites: 15 patch/minor bumps incl. clap 4.6.5, russh 0.62.5, time 0.3.55, wasmparser/wat 0.255; top-level: serde 1.0.229, futures 0.3.33, tokio-family patches, etc.). No exact pins exist; behind-latest majors (reqwest 0.13, tokio-tungstenite 0.30, libsql 0.9, rand 0.10, wasmtime 47) remain deferred per CRATE_AUDIT_v2.0.4.0 rationale (see items #2/#3/#5). Verified: `cargo check` default + all-features + xmpp-bridge + 5 native satellites all exit 0; WASM channel/tool crates lockfile-only per precedent. CI/CD: no external crate versions are pinned in `.gitlab/` (only Rust 1.96.1 toolchain + cargo-deny 0.20.2 installer), `validate_config.py` passes — no CI changes required.
2. [ ] ensure crates are updated
3. [ ] identify crates which have not been updated which should be - put in a new doc
4. [x] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.4. we need bump to 2.0.5 - ensure CI/CD is also updated for the new crate versions — DONE (2026-08-05): bumped all 10 LunarWing-owned packages (`lunarwing`, four workspace crates, `xmpp-bridge`, and four WASM channels) plus seven internal dependency constraints from 2.0.4 to 2.0.5; refreshed six lockfiles with package-version-only deltas. Bumped all four channel registry patch versions as required by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; GitLab config validation passes. Verified default and all-feature workspace checks, bridge and all four channel checks, locked resolution, version policy, and rustfmt under the six-core build cap.
5. [ ] ensure EXTERNAL crates are updated
6. [ ] take look at Advisory Status (`deny.toml`) in docs/ops/CRATE_AUDIT_v2.0.3.0.md ( and docs/ops/CRATE_AUDIT_v2.0.4.0.md ) and install cargo-deny using rustup, cargo, then remove items marked as`now unblocked`
7. [ ] DarkIRC secure staged key exchange. H1/H2 fixed and verified; H3 partially fixed, with live isolation and leak evidence still required. Full boundary, audit ledger, validation, blockers, and next steps: [`DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md`](DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md). Reference branch: darkirckeyexch8/cmc/v2.0.4.0
8. [ ] self healing capability expansion - see prev attempt for reference: shexpansion/cmc/v2.0.4.0
9. [ ] Implement additional UI fixes in document in docs/ops — `docs/ops/UI_FIX_SUGGESTIONS.md`
10. [ ] review darkirc key exchange implementation (only if completed)
11. [ ] work on coordination improvements (there is doc in docs/ops with info and list of known issues which has been updated recently)
12. [ ] come up with codename ideas for release. put into new doc
13. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
14. [ ] update README.md in repo root (if necessary)

---
