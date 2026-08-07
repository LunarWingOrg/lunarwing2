# PRE-RELEASE CHECKLIST for LunarWing v2.0.6.0

## Codename: UNDECIDED

**Open TODOs (v2.0.6.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] implement proposal defined here - docs/proposals/lunarvoice-onnx-proposal-2.0.6.0.md
2. [ ] work on this issue: Bug Report (Sunburst BUG-003) Summary Mission thread goal pollution — mission-spec text leaking into insight extraction. Severity High Source Consolidated from Sunburst's open bug list, last updated 2026-08-01 on build 2.0.3.0.
3. [ ] MCP part 2 work: host sandboxing (see docs for info)
4. [ ] Continue with self healing capability expansion.... Last known status:  — DONE (2026-08-05): ported `shexpansion/cmc/v2.0.4.0` (commit `d1d3c52`) onto v2.0.5.0. Self-heal watchdog v2.1.0 gains: per-service escalation cooldown (SHA-256 collision-resistant sidecars, armed only after confirmed notifier delivery), retryable failed/deferred escalation delivery, state-preserving dry-run, flap restart history surviving recovery, opt-in NetBird readiness check (monitor-only unless explicitly delegated via `SELF_HEAL_NETBIRD_REMEDIATE=true`), matched `SELF_HEAL_SYSTEMD_SCOPE` probing/remediation for base units, LunarVision container remediation (`lunarwing-vision-<t>` units; active-but-unhealthy systemd containers become critical; OpenRC tenant mapping preserved), and persistent OpenRC worker stop intent that removes stale direct/babysitter runlevel links and prevents health remediation from resurrecting paused or unselected workers. mt-admin health.env pins system scope + NetBird-disabled defaults; CI runs aggregate suite once. Verified: `bash ic-infrastructure-health-check/tests/run-all.sh` → 435 passed, 0 failed across all 7 suites. Audit and residual gaps: [`../architecture/SELF_HEAL_COVERAGE_AUDIT_v2.0.4.0.md`](../architecture/SELF_HEAL_COVERAGE_AUDIT_v2.0.4.0.md).
5. [ ] implement mcp part 2 work: general runtime adapters, MCP-over-WASM, and dedicated/shared gateway architecture (see docs for info)
6. [ ] disable lorebooks by default - the are computationally expensive and most users will not use them — DONE 100% (2026-08-07, branch `rarity/kimi-7-20260807-0606`): `LorebookConfig::default()` now sets `enabled: false` in `ic/src/config/agent.rs` (opt back in with `LOREBOOK_ENABLED=true`; env override and validation unchanged). Engine test helper and three workspace activation tests now enable lorebooks explicitly; scheduler/dispatcher test configs already pinned `enabled: false`. Docs updated (`docs/proposals/LOREBOOKS.md` env table default `true`→`false` plus opt-in note, root `README.md`, `ic/src/workspace/README.md`). Verified: `cargo check`, 40 lorebook unit tests + 5 `config::agent` tests, clippy `--all --benches --tests --examples -D warnings`, rustfmt.
7. [ ] mcp part 2 work worker-local placement (see docs for info)
8. [x] (S) Review implementation of item 5 from docs/ops/35d44998cadf3e6d.md - u may clone ironclaw gh repo for reference - do not cokmit ic repo - needs more testing and refinment for sure especially on low end LLMs
9. [ ] Review and make fixes to MCP part two implementation (see mcp documents first)
10. [ ] FIX CI/CD ISSUES
11. [ ] review previous mcp work for part two and fix anything if something is found out to be weird or broken
12. [ ] come up with codename ideas for release. put into new doc
13. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.6.0 explaining all relevant changes since v2.0.5.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.6.0.md` and should be written to the ROOT of the repo
14. [ ] update README.md in repo root (if necessary)

---
