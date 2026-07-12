# AGENT PRE-RELEASE CHECKLIST for 2.0.0.0 Codename `?` (Unknown at this time)

**Open TODOs (2.0.0.0) — To be done before release**

> **NOTE:** Edit the dev autonomous loop routine to use this file. Ensure each checkbox is checked off in the corresponding branch before committing, pushing, and opening a PR. Items are listed in rough recommended order but are not strictly sequential unless noted.
> **NOTE:** Ensure that agent can still read file.

---

1. [x] Ensure references to 1.2.0 in the code and documentation are replaced by 2.0.0 (where applicable only of course)
2. [x] Create ov dir ( cd {this repo} && mkdir -p docs/ov/ )
3. [x] Bump crates version from 1.1.9 to 2.0.0
4. [x] Make sure you DIDNT MISS ANY CRATES. RELEVANT LUNARWING crates and things such as xmpp bridge and wasm channels must have 2.0.0 version, NOT 1.1.9 or 1.1.8
5. [ ] Multiple steps here for this one: First, read the blog post: https://blog.lunarwing.org/2026/07/12/lunarwingv2-the-next-frontier-of-private-self-hosted-ai-agents/ — THEN: Analyze the features not yet included in this AGENT_GOALS_2.0.0.0.md and document all of them into a new document under docs/proposals please.
6. [ ] Drop legacy `ironclaw-agent-v1` subprotocol offer from the daemon (delete the `SUBPROTOCOL_LEGACY` offer in `ic/src/orchestrator/external_worker.rs`) and `git rm` the 1.1.9-only repo-root compat symlinks `ironclaw_weechat_wss`, `darkirc_channel_for_ironclaw` (deployed tenants must have re-run mt-admin unit regen by then)
7. [ ] LunarWing Web UI performance overhaul - lot of issues with UI - laggy, buttons dont animate, etc
8. [x] LunarWing Web MT admin setup integration (part 2 of an earlier plan discussed some time ago) - started - go see: lunarwing_mt_onboard_web/ - this is being worked on by another agent or human
9. [ ] Write up a short doc with details of currently open PRs and Issues. Save it to docs/ops
10. [ ] Inspect status of cargo crates and create documented report of any crates that might still need to be updated. Verify if the info dump below is still correct, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?

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

11. [ ] Run all cargo tests — full `--all-features --no-fail-fast` run: lib 4087 passed/0 failed/4 ignored; all integration binaries + doctests pass except the 6 known-deferred (4 `multi_tenant_system_prompt` architectural, 2 `e2e_advanced_traces` bootstrap-greeting). See `docs/proposals/CARGO_TESTS_FIX.md`.
12. [ ] Fix any remaining broken cargo tests and ensure updated documentation. Create (or rewrite) new tests if necessary. then re-run cargo tests to ensure — Fixed the 3 stale `lib` failures this cycle: `registry::embedded::tests::test_load_embedded_parses` (github→ssh sentinel), `cli::tests::test_help_output` + `test_long_help_output` (accepted rebranded insta snapshots). Lib re-run: 4087 passed/0 failed. The 6 remaining failures are documented known-deferred (architectural / harness), not regressions.
13. [ ] Verify below Gentoo Issue, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?

    <details>
    <summary><b>INFO DUMP — Gentoo WeeChat relay-api</b></summary>

    Issue: -relay-api is disabled (the API protocol needs cJSON). emerge needs cjson.

    Fix: (commands)
    echo "net-irc/weechat relay-api" | sudo tee -a /etc/portage/package.use/weechat
    sudo emerge --oneshot --changed-use net-irc/weechat

    Document the accuracy of this issue.

    </details>

14. [ ] Verify (only) if the information below is accurate, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed? Refer to infodump file located at: `docs/ops/WEECHAT_SERVICES_FOR_MT_INFODUMP.md`
15. [ ] dark irc key exchange. automate the process secruely. ** For this task I want you to help plan it out. Write up a doc in docs/proposals
16. [ ] memory_impl: implement third party memory cleaning, de-duping, correction routines into project. I've created several advanced memory de-duplication routines on my own which are being used across three production agents (on 1.1.2). My goal is to either integrate these routines into LunarWing directly, or make it easy for new users to import them. I'll look into adding more to this idea in this issue at some point. kind of just a stub for the timebeing. ** For this task I want you to help plan it out. Write up a doc in docs/proposals **
17. [x] Work on integrated testing routing - checked this off because not giving auto-dev-loop permission for this
18. [ ] MCP additions: (please reference the following branch for a hint on getting started, old failed implementation from previous opencode worker: faility/failed-partial-old-item-3-20260711-0601) - There are two sections below. You must read BOTH of them (as well as reference the old failed implementation from previous failed opencode worker). Once you have, follow the following instructions: One of the sections is `Recommended List — Pick ONE, implement it, check off item 18` Pick ONE from the `Recommended List — Pick ONE, implement it, check off item 18` list below, implement it, check off this box. Two Informational Sections following this sentence:
    <details>
    <summary><b>DONE — First-class host-local stdio MCP installation</b></summary>

    Implemented first-class host-local stdio MCP installation across LunarWing:
    - Added typed stdio MCP registry manifests with command, structured args, and non-secret env.
    - Added a shared validated ExtensionManager::install_mcp_config persistence path.
    - Extended conversational tool_install and the web API to accept stdio configuration.
    - Added HTTP/stdio controls to the web MCP settings UI.
    - Added stdio support to lunarwing registry list/info/install, including --force handling.
    - Exposed transport and command metadata when listing installed MCP servers.
    - Treated stdio servers as requiring no OAuth authentication.
    - Added validation for empty commands, NUL bytes, and invalid environment names/values.
    - Ensured removal stops the managed child process before deleting its configuration.
    - Preserved existing HTTP MCP and registry precedence behavior.
    - Kept installation separate from execution: installation stores configuration; activation starts the process and discovers tools.
    - Updated the architecture proposal, historical supergateway note, and MCP documentation with neutral local-files examples.

    Worker-local execution, automatic npm/pip installation, secret injection through stdio environment variables, and gateway changes were intentionally excluded. All targeted tests, formatting, JavaScript syntax checks, and cargo check passed.

    </details>

    <details>
    <summary><b>Recommended List — Pick ONE, implement it, check off item 18</b></summary>

    Recommended Next:

    1. MCP deactivate/re-enable
       - Stop the child, unregister its tools, and preserve configuration.
       - Persist enabled = false so restart does not relaunch it.
       - Add tool_deactivate, API, and web controls.
       - This completes the lifecycle without involving WASM or workers.

    2. Diagnostics and command preflight
       - Extend doctor/status with transport, enabled state, and executable availability.
       - Validate absolute commands or resolve commands through PATH.
       - Report spawn and negotiation failures in the installed-extension response.
       - Do not execute anything during installation.

    3. Registry validation
       - Add a validation test or registry validate command covering:
         - exactly one of url or transport
         - valid stdio command/args/env
         - auth: none for stdio
         - duplicate names and unsupported transport types
       - This is almost entirely isolated to registry code and CI.

    4. In-place configuration updates
       - Let users edit command, args, env, or URL without remove/reinstall.
       - If active, require explicit restart confirmation.
       - Preserve existing registry precedence and approval rules.

    5. Focused integration coverage
       - Exercise the real install API through the router.
       - Verify install → list → activate failure reporting → deactivate → remove.
       - Add a browser-level check for HTTP/stdio mode switching and mobile layout.

    Useful, Slightly Larger (optional):
    - Optional stdio working directory (cwd) with path validation.
    - Per-server startup and request timeouts.
    - A visible risk label explaining that host-local processes are unsandboxed.
    - Better process cleanup when a spawn replaces an existing managed transport.
    - Tenant/owner selection for mcp add and registry install; today the CLI persistence path still assumes the default owner.

    Defer For Now:
    Worker-local MCP, automatic npm/pip installation, secret injection through process environment, a general runtime-adapter refactor, gateway integration, and automatic crash restart all increase the security or lifecycle surface materially. Recommended: implement deactivate/re-enable, diagnostics, registry validation, and integration tests as one contained follow-up. That provides a complete and inspectable host-local lifecycle before introducing another execution placement.

    </details>

19. [ ] Finish going through all the documents under architecture directory in docs/ and update all outdated documentation. Then, consolidate documents if possible.
20. [ ] Go through all documents under bugs directory in docs/ and update all outdated documentation. Then, consolidate documents.
21. [ ] Go through all documents under proposals directory in docs/ and update all outdated documentation. Then, consolidate documents if deemed necessary.
22. [ ] Go through all documents under reviews directory in docs/ and update all outdated documentation. Then, consolidate documents.
23. [ ] Go through all documents under guides directory in docs/ and update all outdated documentation. Then, consolidate documents.
24. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.0.0 explaining all relevant changes since v1.1.9.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/release for reference as to how to write up this document. The codename for this release is: `Unknown` — The file you write will be RELEASE-v2.0.0.0.md and should be written to the ROOT of the repo.
25. [ ] Improve accuracy of RELEASE-v2.0.0.0.md

---
