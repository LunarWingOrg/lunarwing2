# PRE-RELEASE CHECKLIST for LunarWing v2.0.4.0

## Codename: Odoshi (威し)

**Open TODOs (v2.0.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

###### (S) is usually noted to denote tasks that can be temporarily deferred

---

1. [x] (S) DarkIRC secure staged key exchange. H1/H2 fixed and verified; H3 partially fixed, with live isolation and leak evidence still required. Full boundary, audit ledger, validation, blockers, and next steps: [`DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md`](DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md). Reference branch: darkirckeyexch8/cmc/v2.0.4.0
2. [x] (S) self healing capability expansion - see prev attempt for reference: shexpansion/cmc/v2.0.4.0
3. [x] Suggest additional UI fixes in a new document in docs/ops — updated `docs/ops/UI_FIX_SUGGESTIONS.md` with additional findings
4. [x] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs - prev attempt that may be referenced as it was fairly accurate at time of creation: rarity/kimi-31-20260801-0601
5. [x] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
6. [x] update README.md at repo root with any missing feature information and other necessary information. remove unnecessary technical information — completed 2026-08-04: added source-verified v2 feature coverage, simplified setup and development guidance, removed stale placeholders and redundant links, and validated local links plus the bounded Rust compile check.
7. [x] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`Odoshi (威し)` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md` — completed 2026-08-04: first draft created at repository root from the `v2.0.3.0..be798a9` source range with current compatibility, upgrade, verification, and known-issue sections; local links, release-version policy, and the bounded Cargo check passed.
8. [x] update anything outdated in docs/proposals — completed 2026-08-04: reconciled coordination, external-worker/Nanocode retirement, Kawarimi live validation, CI, Software Builder, supervision, and historical upgrade claims against source; independently rechecked runtime boundaries including Engine V2-only coordination gating, process-local responder claims, successful staged-import PostgreSQL behavior, connection-only worker failover, the supported babysitter set, and active GitLab pipeline evidence; synchronized the guarded coordination bundle; refreshed the complete proposal index; validated all proposal links/JSON; and passed targeted tests plus bounded `cargo check`
9. [x] update anything outdated that is in docs/ops — completed 2026-08-05: source-audited all 92 operations files; refreshed active v14 registry, tenant/service selection, proxy, worker, WeeChat, DarkIRC, XMPP, GitLab CI, release, and upgrade guidance; marked superseded snapshots and scratch pointers explicitly historical; and validated links, code fences, command references, shell syntax, and targeted mt-admin tests.
10. [ ] update anything outdated in docs/guides - there are a ton of guides in here which have not been updated since v1. many are no longer relevant. they can be placed into archive: docs/guides/history
11. [ ] final pass over release notes and update any inaccuracies and inconsistencies

---
