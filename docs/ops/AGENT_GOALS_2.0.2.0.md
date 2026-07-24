# PRE-RELEASE CHECKLIST for LunarWing v2.0.2.0 Codename `Dōkutsu`

## Release Codename:  洞窟
## English Context:   Cave

**Open TODOs (v2.0.2.0) — To be done before release**

---
1. [x] Bump crate versions. they are at 2.0.1. we bump to 2.0.2
2. [x] (S) Go over proposals
3. [x] (S) get back to previous refactor mt admin idea. see the section of docs/proposals/MT-ADMIN-DECOMPOSITION.md — review notes section of: `docs/ops/KUMOGAKURE_RECENT_REV_T.md` - you will NOT begin ANY work on this yet. you will simply edit the document already created under docs/proposals called `MT-ADMIN-DECOMPOSITION.md`
4. [x] (S) continue to work on kawarimi adapter (for lack of a better name): create a method for migrating a hermes agent to lunarwing v2 safely. SEE: SECTION: hermes_kawarimi — review notes in docs/ops/KUMOGAKURE_RECENT_REV_T.md for suggestedm improvements/concerns
5. [x] (S) work on weechat to reopen the buffers it had open the last time it exited in a reliable fashion - after a machine reboot or restart-tenant command is issued. FEEDBACK FROM LAST TIME: 19:34:19 wrench │ ### Verdict
One real bug found: OpenRC weechat stop() invokes the helper as root instead of the tenant user, which will fail to find the tmux socket. Medium severity — the graceful stop silently becomes a no-op on OpenRC, though the old kill-session behavior would also be broken (same root-cause: missing su). The systemd path is unaffected. Otherwise: solid feature work. The weechat buffer restore is well-designed (proper wrapper script, good fallback chain, both init systems covered, renderer tests). The test-deferral approach is pragmatic and well-documented. You may attempt to use the following work from failed/partial work on these salvaged, incomplete branches: { rarity/item-16-20260722-1201 } { rarity/item-16-20260722-2123 } { rarity/item-16-20260723-0000 }
6. [x] (S) Inspect status of cargo crates
7. [ ] **MCP additions — host-local MCP lifecycle.** The foundation (first-class host-local stdio MCP install) and the registry-validation and diagnostics Recommended-List items were completed and verified on 2026-07-21. Runtime deactivate/re-enable and focused integration coverage were completed on 2026-07-22. Reference the branch here for prior/failed/unverified attempt: rarity/item-8-20260723-0855
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

    - ❌ **In-place configuration updates — NOT STARTED.** Let users edit command/args/env/url without remove+reinstall; if active, require explicit restart confirmation; preserve registry precedence + approval rules. (Only add/remove/toggle/auth/test exist today.) - there is some incomplete work on the branch: rarity/item-19-20260723-0415

    - ✅ **Focused integration coverage — DONE (2026-07-22).**
      - Done: `mcp_extension_lifecycle` e2e (search → install → activate → use) + `mcp_compat/{transport,auth,oauth}` tests — all HTTP mock (`ic/tests/e2e_advanced_traces.rs:511`, `ic/tests/mcp_compat/`).
      - Done 2026-07-22: stdio activate → deactivate → re-enable coverage, live authenticated HTTP deactivate/re-enable coverage, and browser-level Deactivate/Activate control coverage.
      - Done 2026-07-22: one contiguous stdio install → list → activation-failure reporting → deactivate → remove scenario.
      - Done 2026-07-22: browser-level HTTP/stdio mode switching, accessibility state, and mobile overflow/layout coverage.

    **Useful, slightly larger (optional):** stdio working directory (cwd) with path validation; per-server startup/request timeouts; visible "host-local processes are unsandboxed" risk label; better process cleanup when a spawn replaces an existing managed transport; tenant/owner selection for `mcp add` + registry install (CLI persistence still assumes the default owner).

    **Defer for now:** worker-local MCP, automatic npm/pip install, secret injection through process env, general runtime-adapter refactor, gateway integration, automatic crash restart — each materially increases the security/lifecycle surface.
    </details>
8. [x] (S) dark irc key exchange. automate the process secruely. For this task I have already prepared a document you can use for implementation: /docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md -  There is also substantial work on this branches already DONE: feat/darkirc-key-exchange-v1 - You can use the following document for reference on next steps: docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md You may attempt to use the following work from failed/partial work on the branch: rarity/item-20-20260722-1501
9. [ ] review recent changes in onboard ui and see if there are issues
10. [x] (S) review kawarimi adapter (item 15)
11. [x] (S) review darkirc key exchange implementation
12. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
13. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
14. [ ] update any ops docs in docs/ops that may contain outdated information. verify against code rather than docs
15. [ ] update any proposals docs in docs/proposals that may contain outdated information. verify against code rather than docs
16. [ ] update docs/README.md
17. [ ] update README.md at repo root
18. [x] come up with codename ideas for release
19. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.2.0 explaining all relevant changes since v2.0.1.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is: `Dōkutsu` — The file you write will be RELEASE-v2.0.2.0.md and should be written to the ROOT of the repo
20. [ ] Improve accuracy of the first draft of release notes (from previous task) - RELEASE-v2.0.2.0.md

---

