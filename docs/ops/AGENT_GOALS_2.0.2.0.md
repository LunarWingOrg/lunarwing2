# PRE-RELEASE CHECKLIST for LunarWing v2.0.2.0 Codename `Unknown`

## Release Codename:  UNKNOWN
## English Context:   UNKNOWN

**Open TODOs (v2.0.2.0) — To be done before release**

---

1. [x] For docs/ops/WEECHAT_SERVICES_VERIFICATION.md see if there is anything in the code we can improve and do it
2. [x] CHPAR-001 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
3. [x] CHPAR-002 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
4. [x] CHPAR-003 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
5. [x] CHPAR-004 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
6. [x] CHPAR-005 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
7. [x] CHPAR-006 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
8. [x] CHPAR-007 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
9. [x] CHPAR-008 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
10. [x] X
11. [x] CHPAR-011 item from docs/plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md
12. [ ] Take a look at docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md - Identify the top 2 HIGHEST PRIORITY things to implement from candidate additions. Create a single document under docs/proposals to plan these 2 additions.
13. [ ] Take a look at docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md - Identify the top 3 EASIEST things to implement from candidate additions. Create a single document under docs/proposals to plan these 3 additions.
14. [ ] get back to previous refactor mt admin idea. see the section of docs/proposals/MT-ADMIN-DECOMPOSITION.md — review notes section of: `docs/ops/KUMOGAKURE_RECENT_REV_T.md` - you will NOT begin ANY work on this yet. you will simply edit the document already created under docs/proposals called `MT-ADMIN-DECOMPOSITION.md`
15. [ ] continue to work on kawarimi adapter (for lack of a better name): create a method for migrating a hermes agent to lunarwing v2 safely. SEE: SECTION: hermes_kawarimi — review notes in docs/ops/KUMOGAKURE_RECENT_REV_T.md for suggestedm improvements/concerns
16. [ ] work on weechat to reopen the buffers it had open the last time it exited in a reliable fashion - after a machine reboot or restart-tenant command is issued. FEEDBACK FROM LAST TIME: 19:34:19 wrench │ ### Verdict
One real bug found: OpenRC weechat stop() invokes the helper as root instead of the tenant user, which will fail to find the tmux socket. Medium severity — the graceful stop silently becomes a no-op on OpenRC, though the old kill-session behavior would also be broken (same root-cause: missing su). The systemd path is unaffected. Otherwise: solid feature work. The weechat buffer restore is well-designed (proper wrapper script, good fallback chain, both init systems covered, renderer tests). The test-deferral approach is pragmatic and well-documented.
17. [x] Inspect status of cargo crates and create documented report of any crates that might still need to be updated. Verify if the info dump below is still correct, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?
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
18. [ ] Kestrel created a UNIFIED build script to optimize for speed. Take a look at this. You can find it at: scripts/build-lunarwing.sh - let's continue to work on this and make suggestions and ensure it works properly on your machine first to create nice full builds of LunarWing safely across machines with all kinds of resources... ALSO: We can use (`nproc × 0.75`) instead of nproc. That seems safer to me. According to kestrel:
<details>
<summary><b>Recommendation</b></summary>
kestrel │ Two ways:
**Per-build override** (no code change):
```bash
# Flag
./scripts/build-lunarwing.sh -j $(($(nproc) * 3 / 4))
# Or env var
BUILD_JOBS=$(($(nproc) * 3 / 4)) ./scripts/build-lunarwing.sh
```
**Or change the default in the script** — line 73-74, swap:
```bash
DEFAULT_JOBS=$NPROC
```
to:
```bash
DEFAULT_JOBS=$(( NPROC * 3 / 4 ))
```
The `-j` flag and `BUILD_JOBS` env var always override the default, so you've got flexibility per-machine without touching the script.
  </details>
  
19. [ ] **MCP additions — host-local MCP lifecycle.** The foundation (first-class host-local stdio MCP install) and one Recommended-List item (registry validation) are DONE and verified in code (2026-07-21). Closed 2026-07-21 by completing the Diagnostics / command preflight Recommended-List item (see below). Reference branches for prior/failed attempts: `faility/failed-partial-old-item-3-20260711-0601` and `slopmcp1/codex/upgrade/v2.0.0.0`.
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
    - 🟡 **Deactivate / re-enable — PARTIAL.**
      - Done: CLI `mcp toggle --enable/--disable` persists the `enabled` flag (`ic/src/cli/mcp.rs:576`); startup honors it (`ic/src/app.rs:606` via `enabled_servers()`, `ic/src/tools/mcp/config.rs:387`).
      - Remaining: stop the child + unregister its tools at runtime on disable (today `toggle` only rewrites config, so it takes effect on next restart); conversational `tool_deactivate`; web API + web UI toggle controls.

    - ✅ **Diagnostics / command preflight — DONE (2026-07-21).**
      - Done: `lunarwing doctor` `check_mcp_config()` loads enabled servers and runs config `validate()` (`ic/src/cli/doctor.rs`).
      - Done (added 2026-07-21): reports transport (http/stdio/unix) + enabled state per server, resolves stdio commands through PATH (without executing), and flags enabled stdio servers whose command is missing on PATH. Helpers: `preflight_mcp_server`, `resolve_command_in_path`, `is_executable` in `ic/src/cli/doctor.rs`. Unit tests cover PATH resolution and stdio-disabled missing-command behavior.
      - Intentionally still excluded: spawning the server or running an actual MCP handshake during install — install remains separate from execution.

    - ❌ **In-place configuration updates — NOT STARTED.** Let users edit command/args/env/url without remove+reinstall; if active, require explicit restart confirmation; preserve registry precedence + approval rules. (Only add/remove/toggle/auth/test exist today.)

    - 🟡 **Focused integration coverage — PARTIAL.**
      - Done: `mcp_extension_lifecycle` e2e (search → install → activate → use) + `mcp_compat/{transport,auth,oauth}` tests — all HTTP mock (`ic/tests/e2e_advanced_traces.rs:511`, `ic/tests/mcp_compat/`).
      - Remaining: stdio-based lifecycle test; install → list → activate-failure reporting → deactivate → remove; browser-level HTTP/stdio mode switching + mobile-layout check.

    **Useful, slightly larger (optional):** stdio working directory (cwd) with path validation; per-server startup/request timeouts; visible "host-local processes are unsandboxed" risk label; better process cleanup when a spawn replaces an existing managed transport; tenant/owner selection for `mcp add` + registry install (CLI persistence still assumes the default owner).

    **Defer for now:** worker-local MCP, automatic npm/pip install, secret injection through process env, general runtime-adapter refactor, gateway integration, automatic crash restart — each materially increases the security/lifecycle surface.
    </details>
20. [ ] dark irc key exchange. automate the process secruely. For this task I have already prepared a document you can use for implementation: /docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md -  There is also substantial work on this branches already DONE: feat/darkirc-key-exchange-v1 - You can use the following document for reference on next steps: docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md
21. [ ] update onboard ui to correspond with changes in codebase since v2.0.0.0 - surely some things have been broken at this point - in particular, kawarimi is likely broken now due to new 7z encryption
22. [ ] update any architecture docs in docs/
23. [ ] update any bugs docs in docs/bugs
24. [ ] update any ops docs in docs/ops  
25. [ ] update any proposals docs in docs/proposals
26. [ ] update docs/README.md
27. [ ] update README.md at repo root
28. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.2.0 explaining all relevant changes since v2.0.1.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is: `UNKNOWN` — The file you write will be RELEASE-v2.0.2.0.md and should be written to the ROOT of the repo
29. [ ] Improve accuracy of RELEASE-v2.0.2.0.md

---
