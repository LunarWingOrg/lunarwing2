# PRE-RELEASE CHECKLIST for LunarWing v2.0.3.0

**Open TODOs (v2.0.3.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---
1. [x] Bump crate versions. they are at 2.0.2. we need bump to 2.0.3
2. [x] check cicd logic and suggest improvements in doc in docs/ops - this doc is outdated: docs/ops/CICD_LOGIC_REVIEW.md - so worth updating....
3. [x] suggest items that should be added to goals list that arent. put in a new doc in docs/ops
4. [x] (S) Go over proposals defer
5. [x] (S) get back to previous refactor mt admin idea. see the section of docs/proposals/MT-ADMIN-DECOMPOSITION.md — review notes section of: `docs/ops/history/v2.0.3.0/KUMOGAKURE_REVIEW.md` - you will NOT begin ANY work on this yet. you will simply edit the document already created under docs/proposals called `MT-ADMIN-DECOMPOSITION.md`
6. [x] continue to work on kawarimi adapter (for lack of a better name): create a method for migrating a hermes agent to lunarwing v2 safely. SEE: SECTION: hermes_kawarimi — review notes in docs/ops/history/v2.0.3.0/KUMOGAKURE_REVIEW.md for suggestedm improvements/concerns
7. [x] Work on weechat to reopen the buffers it had open the last time it exited in a reliable fashion - after a machine reboot or restart-tenant command is issued. FEEDBACK FROM LAST TIME: 19:34:19 wrench │ ### Verdict: One real bug found: OpenRC weechat stop() invokes the helper as root instead of the tenant user, which will fail to find the tmux socket. Medium severity — the graceful stop silently becomes a no-op on OpenRC, though the old kill-session behavior would also be broken (same root-cause: missing su). The systemd path is unaffected. Otherwise: solid feature work. The weechat buffer restore is well-designed (proper wrapper script, good fallback chain, both init systems covered, renderer tests). The test-deferral approach is pragmatic and well-documented. You may attempt to use the following work from failed/partial work on these salvaged, incomplete branches: { rarity/item-16-20260722-1201 } { rarity/item-16-20260722-2123 } { rarity/item-16-20260723-0000 }
8. [x] Inspect status of cargo crates. Suggest ones that should be updated. put in a file. — see `docs/ops/history/v2.0.3.0/CRATE_AUDIT.md`
9. [x] implement priorirty 1 items in docs/proposals/ENGINE_V2_POLISH_v2.0.3.0.md
10. [x] implement tier 1 updates in docs/ops/history/v2.0.3.0/CRATE_AUDIT.md
11. [x] implement priority 2 items in docs/proposals/ENGINE_V2_POLISH_v2.0.3.0.md
12. [x] implement tier 2 updates in docs/ops/history/v2.0.3.0/CRATE_AUDIT.md
13. [x] implement priority 3 items in docs/proposals/ENGINE_V2_POLISH_v2.0.3.0.md
14. [x] take look at Advisory Status (`deny.toml`) in docs/ops/history/v2.0.3.0/CRATE_AUDIT.md and install cargo-deny using rustup, cargo, then remove items marked as `now unblocked`
15. [x] address p0 issues in docs/ops/history/v2.0.3.0/GOAL_GAP_SUGGESTIONS.md
16. [x] address 2. Priority findings in docs/ops/CICD_LOGIC_REVIEW.md
17. [x] address p1 issues in docs/ops/history/v2.0.3.0/GOAL_GAP_SUGGESTIONS.md
18. [x] address 3. Parity gaps vs. the plan (Phase 1 scope) in docs/ops/CICD_LOGIC_REVIEW.md
19. [x] (S) **MCP additions — host-local MCP lifecycle.** The foundation (first-class host-local stdio MCP install) and the registry-validation and diagnostics Recommended-List items were completed and verified on 2026-07-21. Runtime deactivate/re-enable and focused integration coverage were completed on 2026-07-22. In-place configuration updates were completed on 2026-07-24. Reference the branch here for prior/failed/unverified attempt: rarity/item-8-20260723-0855
    <details>
    <summary><b>✅ DONE (verified 2026-07-21) — Foundational: first-class host-local stdio MCP installation</b></summary>
    Confirmed present in code with references:
    - Typed stdio MCP registry manifests (command, structured args, non-secret env; mutually exclusive with url) — `ic/src/registry/manifest.rs` (`McpManifestTransport::Stdio`, `ExtensionSource::McpStdio`).
    - Shared validated `ExtensionManager::install_mcp_config` persistence path — `ic/src/extensions/manager.rs`.
    - Conversational `tool_install` and web API accept stdio configuration — `ic/src/tools/builtin/extension_tools.rs:102`, `ic/src/channels/web/handlers/extensions.rs:118`, `ic/src/channels/web/server.rs:2150`.
    - HTTP/stdio controls in the web MCP settings UI — `ic/src/channels/web/static/app.js:4614` (`setMcpInstallTransport`, `mcp-http-fields`/`mcp-stdio-fields`).
    - stdio support in `lunarwing registry list/info/install` (incl. `--force`) plus `mcp add` stdio — `ic/src/cli/registry.rs`, `ic/src/cli/mcp.rs`.
    - Transport + command metadata exposed when listing installed MCP servers.
    - stdio servers require no OAuth — `ic/src/tools/mcp/config.rs:250` (`requires_auth()` false for stdio/unix).
    - Validation for empty commands, NUL bytes, invalid env names/values — `ic/src/tools/mcp/config.rs:174-193`.
    - Removal stops the managed child before deleting config — `ic/src/extensions/manager.rs:1144` (unregister tools → drop client → `shutdown(name)` → delete config).
    - Existing HTTP MCP + registry precedence preserved; install kept separate from execution (install stores config; activation spawns + discovers tools).
    Intentionally excluded: worker-local execution, automatic npm/pip install, secret injection through stdio env vars, gateway changes.
    </details>
    <details>
    <summary><b>Recommended List — status (finish ONE incomplete item to close #19)</b></summary>
    Status verified against code on 2026-07-21:
    - ✅ **Registry validation — DONE.** `registry validate` CLI command + `ic/src/registry/validation/mod.rs` (+ tests). Covers exactly one of url/transport, valid stdio command/args/env, `auth: none` for stdio, duplicate names, and unsupported transport types.
    - ✅ **Deactivate / re-enable — DONE (2026-07-22).**
      - `ExtensionManager` now serializes each server's lifecycle transition, tracks exact MCP tool ownership, enforces the activating owner at execution time, persists the desired `enabled` state with atomic DB updates or a disk lock, and explicitly closes transports, managed stdio children, HTTP session state, and pending auth flows on deactivation.
      - Reactivation creates a fresh client, renegotiates MCP, rediscovers tools, and restores `enabled = true`; activation failures clean up spawned runtime resources.
      - Conversational `tool_deactivate`, authenticated `POST /api/extensions/{name}/deactivate`, and MCP-panel Deactivate/Activate controls expose the nondestructive lifecycle. Extension status reports desired `enabled` separately from live `active`.
      - `lunarwing mcp toggle [--enable|--disable]` applies an explicit or atomic invert transition through the authenticated gateway when available. Its explicit `--offline` mode, or a reported gateway-unavailable fallback, persists the change for next startup.
      - Focused coverage includes stdio deactivate/re-enable with process and exact-tool cleanup, client/session shutdown, web handler persistence, browser control behavior, and authenticated HTTP MCP reactivation without reinstalling.

    - ✅ **Diagnostics / command preflight — DONE (2026-07-21).**
      - Done: `lunarwing doctor` `check_mcp_config()` loads enabled servers and runs config `validate()` (`ic/src/cli/doctor.rs`).
      - Done (added 2026-07-21): reports transport (http/stdio/unix) + enabled state per server, resolves stdio commands through PATH (without executing), and flags enabled stdio servers whose command is missing on PATH. Helpers: `preflight_mcp_server`, `resolve_command_in_path`, `is_executable` in `ic/src/cli/doctor.rs`. Unit tests cover PATH resolution and stdio-disabled missing-command behavior.
      - Intentionally still excluded: spawning the server or running an actual MCP handshake during install — install remains separate from execution.

    - ✅ **In-place configuration updates — DONE (2026-07-24).** Let users edit command/args/env/url without remove+reinstall; if active, require explicit restart confirmation; preserve registry precedence + approval rules. (Only add/remove/toggle/auth/test exist today.) - there is some incomplete work on the branch: rarity/item-19-20260723-0415

    - ✅ **Focused integration coverage — DONE (2026-07-22).**
      - Done: `mcp_extension_lifecycle` e2e (search → install → activate → use) + `mcp_compat/{transport,auth,oauth}` tests — all HTTP mock (`ic/tests/e2e_advanced_traces.rs:511`, `ic/tests/mcp_compat/`).
      - Done 2026-07-22: stdio activate → deactivate → re-enable coverage, live authenticated HTTP deactivate/re-enable coverage, and browser-level Deactivate/Activate control coverage.
      - Done 2026-07-22: one contiguous stdio install → list → activation-failure reporting → deactivate → remove scenario.
      - Done 2026-07-22: browser-level HTTP/stdio mode switching, accessibility state, and mobile overflow/layout coverage.

    **Useful, slightly larger (optional):** stdio working directory (cwd) with path validation; per-server startup/request timeouts; visible "host-local processes are unsandboxed" risk label; better process cleanup when a spawn replaces an existing managed transport; tenant/owner selection for `mcp add` + registry install (CLI persistence still assumes the default owner).

    **Defer for now:** worker-local MCP, automatic npm/pip install, secret injection through process env, general runtime-adapter refactor, gateway integration, automatic crash restart — each materially increases the security/lifecycle surface.
    </details>
20. [x] (S) dark irc key exchange. automate the process securely. For this task I have already prepared a document you can use for implementation: /docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md -  There is also substantial work on some branches already done - You can use the following document for reference on next steps: docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md You may attempt to use the following work from failed/partial work on the following branches: rarity/item-20-20260722-1501 and darkirckeyex/aragents/v2.0.3.0 and feat/darkirc-key-exchange-v1
21. [x] (S) continue to work on external workers, in particular for opencode
22. [x] (S) review kawarimi adapter. make improvements (only if necessary)
23. [x] Review new worker container (kimicode). create review document in docs/reviews or docs/ops
24. [x] (S) Refactor mt admin. The existing document plans have SERIOUS issues. Therefore, I'm going with a COMPLETELY different plan for this. Please see the following branch: mtadminrefactor/meta4/v2.0.3.0
25. [x] Suggest additional UI fixes a new document in docs/ops — update docs/ops/UI_FIX_SUGGESTIONS.md with any additional findings — DONE 2026-07-29 (branch aj/kimi-25-20260729-2104): second-pass audit of `ic/src/channels/web/static/` added 10 verified findings — M5 jobs table 6-header/5-cell column mismatch, M6 history messages ignore server `started_at`/`completed_at` (all show page-load time), M7 no 401/403 auth-expiry path (infinite SSE reconnect), M8 restart loader no timeout/escape, M9 escapeHtml not attribute-safe in quoted attrs (CSP-bounded), L5 hljs never loaded (dead highlighting), L6 hover-only gateway/TEE popovers, L7 polling in hidden tabs, L8 missing rel=noopener, L9 window.prompt in publish flow; CSP tightening note added under H1; implementation order updated
26. [x] Review recent work on Further polishing of Lunarvision AND XMPP file sharing integration, see documents about this if unsure. create new doc detailing findings in docs/ops called XMPP_FT_REVIEW_A.md (yes, it is intentional to have this goal defined twice)
27. [x] Review recent changes for: Gut proprietary tunnel providers and add support for netbird. improve if necessary
28. [x] ensure dependency list is updated for gentoo and include archlinux dependency list if not present as well. be sure to include python packages not listed in these dep lists currently
29. [x] review darkirc key exchange implementation (only if item 20 was completed). if #20 is not done, check this off — #20 not completed, checked off per instruction
30. [x] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs — DONE 2026-07-28 (codex worker, branch feat/item-30-arch-docs-refresh): 6 of 9 docs updated (+70/−46), 3 audited clean (SSH_AGENT_HARNESS, SSH_DELIVERY_MECHANISMS, XMPP_FILE_TRANSFERS); 13 changed claims re-verified against code
31. [x] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
32. [x] (S) update any ops docs in docs/ops that may contain outdated information. verify against code rather than docs
33. [x] write up proper plan document in docs/proposals for integrating docs/proposals/MULTIAGENTTEAMORCHESTRATION into the next release, 2.0.4.0
34. [x] (S) Work on reflex compiler. see docs/proposals/REFLEXCOMPILERANDSOFTWAREBUILDERUPDATESNEEDED in docs/proposals - ALSO, see the following branches: rc2/aragents/v2.0.3.0 and anattemptatreflexcompilerv2 for work as inspiration
35. [x] update any proposals docs in docs/proposals that may contain outdated information. verify against code rather than docs
36. [x] update docs/README.md — DONE 2026-07-28 (codex worker, branch feat/item-36-docs-readme): 75 missing entries added, 4 broken rows removed, 1 renamed link fixed; 262 referenced paths verified with 0 missing; REVIEW_R + CODENAME docs indexed at merge time
37. [x] update README.md at repo root — DONE 2026-07-28 (codex worker, branch feat/item-37-root-readme): added XEP-0363 file attachments, WeeChat buffer restoration, and host-local stdio MCP lifecycle bullets; NetBird named in integrations; Gentoo/Arch package lists linked in host prep; all 22 repo-relative links verified; final pass still due under item 40
38. [x] come up with codename ideas for release. put into new doc. — DONE
39. [x] Write up FIRST DRAFT release notes (at root of repo) for v2.0.3.0 explaining all relevant changes since v2.0.2.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is: `Sashiko (刺し子)` — The file you write will be RELEASE-v2.0.3.0.md and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) - RELEASE-v2.0.3.0.md
40. [x] review release notes
41. [x] Propose method of fixing self improving skills and create a new doc in docs/ops with your proposal - See notes here for information: docs/ops/history/SIS_FIELD_NOTES.md — DONE 2026-07-29 (grafted from branches `k-sis-m-2/2.0.3.0` + `g-sis-m-2/2.0.3.0`): `docs/ops/SIS_FIX_PROPOSAL.md`; root-caused 4 breaks (terminal `StateChanged` never broadcast, `trigger_payload` never seeded into mission state, permanent-lease leak, and no mission write path for skill docs — the last being why zero `source: "extracted"` skills existed) and implemented all four, plus persisted-before-broadcast event ordering, gate-pause payload retention, failed-thread diagnosis delivery, per-user insights cadence, and lease rollback on spawn/resume failure; profile-builder confirmed nonexistent (conversation-insights is the equivalent)
42. [x] review README.md AND release notes from item #39 in root of repo and do a final pass. There is one recent addition which is not included in the repo like the work for the new kimicode harness added as an external worker - please add it — DONE 2026-07-29: Kimi Code external worker added to RELEASE-v2.0.3.0.md (blurb, overview, highlights, new Changes subsection, compatibility row, regression coverage); README already covered the worker — added link to docs/ops/WORKER-CONTAINERS.md; all README repo-relative links verified resolving
43. [x] Ensure proposals in docs/proposals are up to date — DONE 2026-07-29 (branch rarity/kimi-43-20260729-0733): audited all 70+ proposal files against code; every file carries a current status header. Fixed 5 stale ones: ENGINE_V2_POLISH P1/P2 marked implemented (verified in source: provenance wired in engine executors, ownership-checked hard delete_mission, name/new_name parser fix, pending-gate sweep + configurable expiry, fire_webhook_mission, bootstrap_all_projects), MT-ADMIN-DECOMPOSITION marked superseded (item #24 chose mtadminrefactor/meta4/v2.0.3.0 mt-admin-lib split, unmerged), DARKIRC_SECURE_KEY_EXCHANGE noted unmerged Phase 1/Stage A+C branch work (rarity/item-20-20260722-1501), REFLEX_COMPILER_V2_UPGRADE_PLAN given missing status header (open; reflex-fix-1..8 unmerged on anattemptatreflexcompilerv2/ocpt1), WEECHAT relay + SELF_IMPROVING_SKILLS_B1 statuses refreshed (item #7 buffer restore, item #41 SIS repairs); docs/README.md index synced
44. [x] redo roadmap found at docs/ops/ROADMAP_2026.md - needs proper updating - the things we discussed and have documents for in docs/proposals which are not already in there should be added for 2.0.4.0 and 2.0.6.0 and 2.0.8.0 - spread them out if possible
45. [x] create a document in docs/ops to give a list of all the things that were changed since 2.0.2.0 and what to test thoroughly on upgraded testing tenants — DONE 2026-07-29 (branch rarity/kimi-45-20260729-1800): `docs/ops/CHANGES_SINCE_2.0.2.0_AND_UPGRADED_TENANT_TESTS.md` — Part 1 inventories all changes since v2.0.2.0 (Engine V2 hardening, NetBird tunnel replacement + V24, WeeChat restore handoff, worker hardening, MCP updates, SIS repair, Study Notes, DarkIRC outbox, web UI, toolchain/deps, docs) sourced from RELEASE-v2.0.3.0.md and git log; Part 2 gives 12 per-area test sections (T0–T11) with concrete mt-admin commands (verbs verified against the script) and a sign-off checklist

---
