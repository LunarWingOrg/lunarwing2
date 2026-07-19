# PRE-RELEASE CHECKLIST for LunarWing v2.0.1.0 Codename `Togishi`

## Release Codename:  研師
## English Context:   Togishi

**Open TODOs (v2.0.1.0) — To be done before release**

---

1. [x] For docs/ops/WEECHAT_SERVICES_VERIFICATION.md see if there is anything in the code we can improve and do it
2. [x] CHPAR-001 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
3. [x] CHPAR-002 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
4. [x] CHPAR-003 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
5. [ ] CHPAR-004 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
6. [x] CHPAR-005 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
7. [ ] CHPAR-006 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
8. [ ] CHPAR-007 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
9. [ ] CHPAR-008 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
10. [ ] CHPAR-009 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
11. [ ] CHPAR-010 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
12. [ ] CHPAR-011 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
13. [ ] CHPAR-012 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
14. [ ] figure out a way for weechat to reopen the buffers it had open the last time it exited in a reliable fashion - after a machine reboot or restart-tenant command is issued
15. [ ] Inspect status of cargo crates and create documented report of any crates that might still need to be updated. Verify if the info dump below is still correct, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?
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
16. [ ] Fix any remaining broken cargo tests and ensure updated documentation. Create (or rewrite) new tests if necessary. then re-run cargo tests to ensure
17. [x] (checked off cuz wanna get back to this another time) MCP additions: ( please reference the following branch for a hint on getting started, old failed implementations from previous opencode/codex workers: faility/failed-partial-old-item-3-20260711-0601 AND slopmcp1/codex/upgrade/v2.0.0.0 ) - There are two sections below. You must read BOTH of them (as well as reference the old failed implementation from previous failed opencode worker). Once you have, follow the following instructions: One of the sections is `Recommended List — Pick ONE, implement it, check off item 18` Pick ONE from the `Recommended List — Pick ONE, implement it, check off item 18` list below, implement it, check off this box. Two Informational Sections following this sentence:
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
    MCP deactivate/re-enable
       - Stop the child, unregister its tools, and preserve configuration.
       - Persist enabled = false so restart does not relaunch it.
       - Add tool_deactivate, API, and web controls.
       - This completes the lifecycle without involving WASM or workers.
       Diagnostics and command preflight
       - Extend doctor/status with transport, enabled state, and executable availability.
       - Validate absolute commands or resolve commands through PATH.
       - Report spawn and negotiation failures in the installed-extension response.
       - Do not execute anything during installation.
       Registry validation — DONE
       - Add a validation test or registry validate command covering:
         - exactly one of url or transport
         - valid stdio command/args/env
         - auth: none for stdio
         - duplicate names and unsupported transport types
       - This is almost entirely isolated to registry code and CI.
       In-place configuration updates
       - Let users edit command, args, env, or URL without remove/reinstall.
       - If active, require explicit restart confirmation.
       - Preserve existing registry precedence and approval rules.
       Focused integration coverage
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
18. [ ] dark irc key exchange. automate the process secruely. For this task I have already prepared a document you can use for implementation: /docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md -  There is also substantial work on this branches already DONE: feat/darkirc-key-exchange-v1 - You can use the following document for reference on next steps: docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md
19. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.1.0 explaining all relevant changes since v2.0.0.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/release for reference as to how to write up this document. The codename for this release is: `Togishi` — The file you write will be RELEASE-v2.0.1.0.md and should be written to the ROOT of the repo.
20. [ ] Improve accuracy of RELEASE-v2.0.1.0.md

---
