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
12. [x] Take a look at docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md - Identify the top 2 HIGHEST PRIORITY things to implement from candidate additions. Create a single document under docs/proposals to plan these 2 additions.
13. [x] Take a look at docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md - Identify the top 3 EASIEST things to implement from candidate additions. Create a single document under docs/proposals to plan these 3 additions.
14. [x] get back to previous refactor mt admin idea. see the section of docs/proposals/MT-ADMIN-DECOMPOSITION.md — review notes section of: `docs/ops/KUMOGAKURE_RECENT_REV_T.md` - you will NOT begin ANY work on this yet. you will simply edit the document already created under docs/proposals called `MT-ADMIN-DECOMPOSITION.md`
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
18. [x] Harden and verify the unified Linux `aarch64`/`x86_64` native build script at `scripts/build-lunarwing.sh`. Completed 2026-07-22.
    <details>
    <summary><b>Completion and verification</b></summary>

    - Automatic parallelism now uses 75% of effective CPUs, never zero, capped by available memory. Affinity and inherited cgroup v1/v2 CPU and memory limits are included.
    - `-j` and `BUILD_JOBS` remain explicit overrides, with validation and warnings when they exceed the safe automatic recommendation. Command-line flags take precedence over environment defaults.
    - Removed all automatic Cargo/Rust process killing and Cargo lock-file deletion. Concurrent invocations of this helper use a canonical-target sidecar `flock`; `--clean` fails closed when safe locking is unavailable.
    - Target paths are created and canonicalized, aliases resolving to `/` are rejected, tmpfs/ramfs and low-disk conditions are reported, and paths containing spaces are preserved.
    - Cargo is invoked without `eval` as `cargo build --locked --bin lunarwing`; release/debug validation, unique logs, dry-run behavior, configured Cargo targets, and failure propagation are covered.
    - The script intentionally supports Linux only. `aarch64` and `x86_64` use the same resource-based policy rather than unreliable architecture-specific resource assumptions.
    - Added `scripts/tests/test-build-lunarwing.sh`: 37 focused checks pass. `bash -n`, ShellCheck warning-level validation, and `git diff --check` also pass.
    - Native `x86_64` verification command: `taskset -c 0-5 ./scripts/build-lunarwing.sh`. Result: automatic `-j4`, 769 crates compiled, successful release build in 11m 5s, 2.8GB target directory, and a stripped 97MB x86-64 ELF binary. `lunarwing --version` reports `2.0.1`.
    - Native execution on an `aarch64` host was not available in this worktree; the shared architecture path and resource calculations are covered by the shell harness.
    </details>
  
19. [x] **MCP additions — host-local MCP lifecycle.** The foundation (first-class host-local stdio MCP install) and the registry-validation and diagnostics Recommended-List items were completed and verified on 2026-07-21. Runtime deactivate/re-enable was completed on 2026-07-22. Reference branches for prior/failed attempts: `faility/failed-partial-old-item-3-20260711-0601` and `slopmcp1/codex/upgrade/v2.0.0.0`.
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

    - ❌ **In-place configuration updates — NOT STARTED.** Let users edit command/args/env/url without remove+reinstall; if active, require explicit restart confirmation; preserve registry precedence + approval rules. (Only add/remove/toggle/auth/test exist today.)

    - 🟡 **Focused integration coverage — PARTIAL.**
      - Done: `mcp_extension_lifecycle` e2e (search → install → activate → use) + `mcp_compat/{transport,auth,oauth}` tests — all HTTP mock (`ic/tests/e2e_advanced_traces.rs:511`, `ic/tests/mcp_compat/`).
      - Done 2026-07-22: stdio activate → deactivate → re-enable coverage, live authenticated HTTP deactivate/re-enable coverage, and browser-level Deactivate/Activate control coverage.
      - Remaining: one contiguous install → list → activation-failure reporting → deactivate → remove scenario; browser-level HTTP/stdio mode switching + mobile-layout check.

    **Useful, slightly larger (optional):** stdio working directory (cwd) with path validation; per-server startup/request timeouts; visible "host-local processes are unsandboxed" risk label; better process cleanup when a spawn replaces an existing managed transport; tenant/owner selection for `mcp add` + registry install (CLI persistence still assumes the default owner).

    **Defer for now:** worker-local MCP, automatic npm/pip install, secret injection through process env, general runtime-adapter refactor, gateway integration, automatic crash restart — each materially increases the security/lifecycle surface.
    </details>
20. [ ] dark irc key exchange. automate the process secruely. For this task I have already prepared a document you can use for implementation: /docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md -  There is also substantial work on this branches already DONE: feat/darkirc-key-exchange-v1 - You can use the following document for reference on next steps: docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md
21. [x] update onboard ui to correspond with changes in codebase since v2.0.0.0 - surely some things have been broken at this point - in particular, kawarimi is likely broken now due to new 7z encryption
22. [ ] update any architecture docs in docs/
23. [ ] update any bugs docs in docs/bugs
24. [ ] update any ops docs in docs/ops  
25. [ ] update any proposals docs in docs/proposals
26. [ ] update docs/README.md
27. [ ] update README.md at repo root
28. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.2.0 explaining all relevant changes since v2.0.1.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is: `UNKNOWN` — The file you write will be RELEASE-v2.0.2.0.md and should be written to the ROOT of the repo
29. [ ] Improve accuracy of RELEASE-v2.0.2.0.md

---
