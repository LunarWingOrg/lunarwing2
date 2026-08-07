# PRE-RELEASE CHECKLIST for LunarWing v2.0.6.0

## Codename: UNDECIDED

**Open TODOs (v2.0.6.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] implement proposal defined here - docs/proposals/lunarvoice-onnx-proposal-2.0.6.0.md
2. [ ] work on this issue: Bug Report (Sunburst BUG-003) Summary Mission thread goal pollution — mission-spec text leaking into insight extraction. Severity High Source Consolidated from Sunburst's open bug list, last updated 2026-08-01 on build 2.0.3.0 (might already be fixed - in that case, confirm)
3. [ ] MAKE V2 ENGINE CHANNELS ON BY DEFAULT FOR ALL i.e.  ENGINE_V2_CHANNELS=xmpp,weechat,darkirc by default should be present in new tenant env - ensure never overwrite existing tenant's value upon upgrade or rebuild or re-render
4. [x] (S) Continue with self healing capability expansion.... Last known status:  — DONE (2026-08-05): ported `shexpansion/cmc/v2.0.4.0` (commit `d1d3c52`) onto v2.0.5.0. Self-heal watchdog v2.1.0 gains: per-service escalation cooldown (SHA-256 collision-resistant sidecars, armed only after confirmed notifier delivery), retryable failed/deferred escalation delivery, state-preserving dry-run, flap restart history surviving recovery, opt-in NetBird readiness check (monitor-only unless explicitly delegated via `SELF_HEAL_NETBIRD_REMEDIATE=true`), matched `SELF_HEAL_SYSTEMD_SCOPE` probing/remediation for base units, LunarVision container remediation (`lunarwing-vision-<t>` units; active-but-unhealthy systemd containers become critical; OpenRC tenant mapping preserved), and persistent OpenRC worker stop intent that removes stale direct/babysitter runlevel links and prevents health remediation from resurrecting paused or unselected workers. mt-admin health.env pins system scope + NetBird-disabled defaults; CI runs aggregate suite once. Verified: `bash ic-infrastructure-health-check/tests/run-all.sh` → 435 passed, 0 failed across all 7 suites. Audit and residual gaps: [`../architecture/SELF_HEAL_COVERAGE_AUDIT_v2.0.4.0.md`](../architecture/SELF_HEAL_COVERAGE_AUDIT_v2.0.4.0.md).
5. [ ] MCP part 2 work: host sandboxing (see docs for info)
6. [ ] go over items in work items. make note of any that are already confirmed fix and should be closed
7. [ ] implement mcp part 2 work: general runtime adapters, MCP-over-WASM, and dedicated/shared gateway architecture (see docs for info)
8. [ ] Research the following and make a new doc in ops with findings: LunarHub / IronHub / Extension Marketplace — Future Direction - What they did: IronHub deep link installs, custom MCP server registration from WebUI with OpenAPI-derived tool discovery bounded by manifest tool budget. Extension lifecycle with CAS-protected mutations, hash-pinned manifests, soft-removal tombstones, restart repair. - What LunarWing needs first: 1 Binary distribution (the identified blocker) — proper packaged releases for LunarWing binaries, 2 Manifest registry — could be hosted on git.lunarwing.org, 3 Install flow with provenance verification — MCP provenance pinning already landing in 2.0.6.0 - Status: Pieces are being built. Marketplace is post-2.0.7.0 at earliest
9. [ ] mcp part 2 work worker-local placement (see docs for info)
10. [x] (S) Review implementation of item 5 from docs/ops/35d44998cadf3e6d.md - u may clone ironclaw gh repo for reference - do not cokmit ic repo - needs more testing and refinment for sure especially on low end LLMs
11. [ ] Review and make fixes to MCP part two implementation (see mcp documents first)
12. [ ] FIX CI/CD ISSUES
13. [ ] review previous mcp work for part two and fix anything if something is found out to be weird or broken
14. [ ] come up with codename ideas for release. put into new doc
15. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.6.0 explaining all relevant changes since v2.0.5.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.6.0.md` and should be written to the ROOT of the repo
16. [ ] update README.md in repo root (if necessary)

---
