# PRE-RELEASE CHECKLIST for LunarWing v2.0.4.0

## Codename: Odoshi (威し)

**Open TODOs (v2.0.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

###### (S) is usually noted to denote tasks that can be temporarily deferred

---

2. [ ] **DarkIRC secure staged key exchange.** H1/H2 fixed and verified; H3 partially fixed, with live isolation and leak evidence still required. Full boundary, audit ledger, validation, blockers, and next steps: [`DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md`](DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md).
4. [ ] self healing capability expansion - see prev attempt for reference: aj/kimi-24-20260801-1203
6. [ ] Suggest additional UI fixes a new document in docs/ops — update`docs/ops/UI_FIX_SUGGESTIONS.md` with any additional findings
7. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs - prev attempt that may be referenced as it was fairly accurate at time of creation: rarity/kimi-31-20260801-0601
8. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
10. [ ] update README.md at repo root
12. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
13. [ ] update anything outdated in docs/proposals
14. [ ] update anything outdated that is in docs/ops
15. [ ] update anything outdated in docs/guides - there are a ton of guides in here which have not been updated since v1. many are no longer relevant. they can be placed into archive: docs/guides/history

___ 


Item 14 completion hardening (2026-08-03): host-local stdio recovery now runs one
cancellable task per server, permits five unstable restart attempts with jittered
bounded backoff, resets attempts after 30 healthy seconds, and opens a visible
circuit instead of restart-storming. Manual lifecycle work cancels stale recovery;
activation rollback, persistence-before-teardown, atomic tool replacement, fresh
secret resolution, and deadline-bounded error-path shutdown close resurrection and
resource-leak races. Scope remains Part 1 host-local lifecycle only; package
installation/provenance, host OS sandboxing, worker placement, generic runtime
adapters, MCP-over-WASM, and dedicated/shared gateway work remain deferred. See
[`MCP_OVERALL_STATUS.md`](MCP_OVERALL_STATUS.md) for concise current status,
remaining validation, and deferred work.

---
