# PRE-RELEASE CHECKLIST for LunarWing v2.0.4.0

**Open TODOs (v2.0.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.3. we need bump to 2.0.4
2. [ ] Per-tenant WeeChat health-glob gate (fix the flap /render-units footgun)
3. [ ] Inspect status of ALL cargo crates. check audit doc in docs/ops and start updating crates and verifying successful build
4. [ ] continue to work on kawarimi adapter (for lack of a better name): create a method for migrating a hermes agent to lunarwing v2 safely. SEE: SECTION: hermes_kawarimi — review notes in docs/ops/KUMOGAKURE_RECENT_REV_T.md for suggestedm improvements/concerns. live verify hermes agent and migrate it to lunarwing as a new tenant
5. [ ] Reflex compiler improvements - Work on reflex compiler. see docs/proposals/REFLEXCOMPILERANDSOFTWAREBUILDERUPDATESNEEDED in docs/proposals - ALSO, see the following branches:`rc2/aragents/v2.0.3.0` and`anattemptatreflexcompilerv2` for work as inspiration
6. [ ] Multi-agent agent team orchestration via XMPP. this requires implementing plan from ruffles in docs/proposals - four phase engine work - please search if unsure - see MULTIAGENTTEAMORCHESTRATION document - see XMPP rate limiting as well
7. [x] Work on weechat to reopen the buffers it had open the last time it exited in a reliable fashion - after a machine reboot or restart-tenant command is issued. FEEDBACK FROM LAST TIME:`19:34:19 wrench │ ### Verdict: One real bug found: OpenRC weechat stop() invokes the helper as root instead of the tenant user, which will fail to find the tmux socket. Medium severity — the graceful stop silently becomes a no-op on OpenRC, though the old kill-session behavior would also be broken (same root-cause: missing su). The systemd path is unaffected. Otherwise: solid feature work. The weechat buffer restore is well-designed (proper wrapper script, good fallback chain, both init systems covered, renderer tests). The test-deferral approach is pragmatic and well-documented.` You may attempt to use the following work from failed/partial work on these salvaged, incomplete branches: { rarity/item-16-20260722-1201 } { rarity/item-16-20260722-2123 } { rarity/item-16-20260723-0000 }
8. [ ] add lorebook support such as sillytavern for agents
9. [x] implement priorirty 1 items in docs/proposals/ENGINE_V2_POLISH_v2.0.3.0.md
10. [x] implement tier 1 updates in docs/ops/CRATE_AUDIT_v2.0.3.0.md
11. [x] implement priority 2 items in docs/proposals/ENGINE_V2_POLISH_v2.0.3.0.md
12. [x] implement tier 2 updates in docs/ops/CRATE_AUDIT_v2.0.3.0.md
13. [x] implement priority 3 items in docs/proposals/ENGINE_V2_POLISH_v2.0.3.0.md
14. [x] take look at Advisory Status (`deny.toml`) in docs/ops/CRATE_AUDIT_v2.0.3.0.md and install cargo-deny using rustup, cargo, then remove items marked as`now unblocked`
15. [x] address p0 issues in docs/ops/GOAL_GAP_SUGGESTIONS.md
16. [x] address 2. Priority findings in docs/ops/CICD_LOGIC_REVIEW.md
17. [x] address p1 issues in docs/ops/GOAL_GAP_SUGGESTIONS.md
18. [x] address 3. Parity gaps vs. the plan (Phase 1 scope) in docs/ops/CICD_LOGIC_REVIEW.md
19. [ ] MCP additions — host-local MCP lifecycle. worker-local MCP, automatic npm/pip install, secret injection through process env, general runtime-adapter refactor, gateway integration, automatic crash restart — each materially increases the security/lifecycle surface.
20. [ ] dark irc key exchange. automate the process securely. For this task I have already prepared a document you can use for implementation:`/docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md` - There is also substantial work on some branches already done - You can use the following document for reference on next steps:`docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md` You may attempt to use the following work from failed/partial work on the following branches:`rarity/item-20-20260722-1501` and`darkirckeyex/aragents/v2.0.3.0` and`feat/darkirc-key-exchange-v1`
21. [ ] continue to work on external workers, in particular for kimicode
22. [ ] review kawarimi adapter. make improvements (only if necessary)
23. [ ] self healing capability expansion
24. [x] (S) Refactor mt admin. The existing document plans have SERIOUS issues. Therefore, I'm going with a COMPLETELY different plan for this. Please see the following branch:`mtadminrefactor/meta4/v2.0.3.0`
25. [ ] Suggest additional UI fixes a new document in docs/ops — update`docs/ops/UI_FIX_SUGGESTIONS.md` with any additional findings
26. [ ] improve Lunarvision AND XMPP file sharing integration, see documents about this if unsure. see`XMPP_FT_REVIEW_A.md` and other docs
27. [ ] improve netbird support
28. [ ] create dependency list for debian, ubuntu, fedora. be sure to include python packages not listed in these dep lists currently
29. [ ] review darkirc key exchange implementation (only if completed). if not done, check this off
30. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
31. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
32. [ ] update any ops docs in docs/ops that may contain outdated information. verify against code rather than docs
33. [x] write up proper plan document in docs/proposals for integrating docs/proposals/MULTIAGENTTEAMORCHESTRATION into the next release, 2.0.4.0
34. [ ] review work on reflex compiler
35. [ ] update any proposals docs in docs/proposals that may contain outdated information. verify against code rather than docs
36. [ ] update docs/README.md
37. [ ] update README.md at repo root
38. [ ] come up with codename ideas for release. put into new doc
39. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
40. [x] proposals
41. [x] Propose method of fixing self improving skills and create a new doc in docs/ops with your proposal - See notes here for information:`docs/ops/SIS.md` — DONE 2026-07-29 (grafted from branches`k-sis-m-2/2.0.3.0` +`g-sis-m-2/2.0.3.0`):`docs/ops/SIS_FIX_PROPOSAL.md`; root-caused 4 breaks (terminal`StateChanged` never broadcast,`trigger_payload` never seeded into mission state, permanent-lease leak, and no mission write path for skill docs — the last being why zero`source: "extracted"` skills existed) and implemented all four, plus persisted-before-broadcast event ordering, gate-pause payload retention, failed-thread diagnosis delivery, per-user insights cadence, and lease rollback on spawn/resume failure; profile-builder confirmed nonexistent (conversation-insights is the equivalent)
42. [ ] review release notes from item 40 -else skip and mark this done
43. [ ] Ensure proposals in docs/proposals are up to date
44. [ ] The roadmap document - which can be found at`docs/ops/ROADMAP_2026.md` - needs proper updating - the things we discussed and have documents for in docs/proposals which are not already in there should be added for 2.0.6.0 and 2.0.8.0 and 2.1.0.0 - spread them out if possible
45. [ ] create a document in docs/ops to give a list of all the things that were changed since 2.0.3.0 and what to test thoroughly on upgraded testing tenants

---
