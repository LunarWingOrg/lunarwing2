# PRE-RELEASE CHECKLIST for LunarWing v2.1.0.0

## Codename: UNKNOWN

**Open TODOs (v2.1.0.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] update all external crates that need updating for entire project. see previous release notes for some examples of such crates - ensure CI/CD is also updated for the new crate versions
2. [ ] update all lunarwing crates from 2.0.9 to 2.1.0 - other crates such as wasm channel/tool crates that dont specifically have these versions must also be updated in order to pass CI/CD requirements
3. [x] (S) **DarkIRC secure staged key exchange Stage A acceptance.** CURRENT STATUS (2026-08-05, commit `fb548a6`): Stage A implementation and source-audit hardening are committed; H1/H2, M1-M4, and L1-L2 are fixed. Local gates are green: 89 core tests, 22 helper tests, full GitLab shell harness, locked `cargo check`, rustfmt, all-feature Clippy with warnings denied, 11 H3 analyzer tests, ShellCheck, and `git diff --check`. Acceptance remains open because historical OpenRC evidence predates immutable release v2. Run current release-v2 H3 on disposable OpenRC and real systemd-user hosts, then final release CI. Stages B-D and delivery remain disabled and separately gated. Full ledger: [`DARKIRC-KEYEXCHANGE-STATUS.md`](../DARKIRC-KEYEXCHANGE-STATUS.md).
4. [x] come up with codename ideas for release. put into new doc — DONE (2026-08-06): added [`v2.0.5.0/CODENAME_IDEAS.md`](v2.0.5.0/CODENAME_IDEAS.md) with ten candidates following the Japanese word/name convention (no repeats of past codenames), themed on this cycle's actual work — DarkIRC staged key exchange Stage A hardening, self-healing watchdog v2.1.0, and dependency/supply-chain maintenance. Top picks: Kagi (鍵), Chiyu (治癒), Hagane (鋼).
5. [x] (S) Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
6. [ ] update README.md in repo root (if necessary)

---
