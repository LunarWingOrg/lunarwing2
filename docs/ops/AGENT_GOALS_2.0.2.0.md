# PRE-RELEASE CHECKLIST for LunarWing v2.0.2.0 Codename `Unknown`

## Release Codename:  UNKNOWN
## English Context:   UNKNOWN

**Open TODOs (v2.0.2.0) — To be done before release**

---

1. [x] Take a look at docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md - Identify the top 2 HIGHEST PRIORITY things to implement from candidate additions. Create a single document under docs/proposals to plan these 2 additions.
2. [x] Take a look at docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/README.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-1.0.0-rc.1-port-analysis.md and docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md - Identify the top 3 EASIEST things to implement from candidate additions. Create a single document under docs/proposals to plan these 3 additions.
3. [x] get back to previous refactor mt admin idea. see the section of docs/proposals/MT-ADMIN-DECOMPOSITION.md — review notes section of: `docs/ops/KUMOGAKURE_RECENT_REV_T.md` - you will NOT begin ANY work on this yet. you will simply edit the document already created under docs/proposals called `MT-ADMIN-DECOMPOSITION.md`
4. [x] continue to work on kawarimi adapter (for lack of a better name): create a method for migrating a hermes agent to lunarwing v2 safely. SEE: SECTION: hermes_kawarimi — review notes in docs/ops/KUMOGAKURE_RECENT_REV_T.md for suggestedm improvements/concerns
5. [x] work on weechat to reopen the buffers it had open the last time it exited in a reliable fashion - after a machine reboot or restart-tenant command is issued. FEEDBACK FROM LAST TIME: 19:34:19 wrench │ ### Verdict
One real bug found: OpenRC weechat stop() invokes the helper as root instead of the tenant user, which will fail to find the tmux socket. Medium severity — the graceful stop silently becomes a no-op on OpenRC, though the old kill-session behavior would also be broken (same root-cause: missing su). The systemd path is unaffected. Otherwise: solid feature work. The weechat buffer restore is well-designed (proper wrapper script, good fallback chain, both init systems covered, renderer tests). The test-deferral approach is pragmatic and well-documented. You may attempt to use the following work from failed/partial work on these salvaged, incomplete branches: { rarity/item-16-20260722-1201 } { rarity/item-16-20260722-2123 } { rarity/item-16-20260723-0000 }
6. [x] Inspect status of cargo crates and create documented report of any crates that might still need to be updated. Verify if the info dump below is still correct, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?
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
    
7. [x] Harden and verify the unified Linux `aarch64`/`x86_64` native build script at `scripts/build-lunarwing.sh`. Completed 2026-07-22.
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
    
8. [ ] **MCP additions — host-local MCP lifecycle.** The foundation (first-class host-local stdio MCP install) and the registry-validation and diagnostics Recommended-List items were completed and verified on 2026-07-21. Runtime deactivate/re-enable and focused integration coverage were completed on 2026-07-22. Reference branches for prior/failed attempts: `faility/failed-partial-old-item-3-20260711-0601` and `slopmcp1/codex/upgrade/v2.0.0.0`.
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
9. [ ] dark irc key exchange. automate the process secruely. For this task I have already prepared a document you can use for implementation: /docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md -  There is also substantial work on this branches already DONE: feat/darkirc-key-exchange-v1 - You can use the following document for reference on next steps: docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md You may attempt to use the following work from failed/partial work on the branch: rarity/item-20-20260722-1501
10. [x] update onboard ui to correspond with changes in codebase since v2.0.0.0 - surely some things have been broken at this point - in particular, kawarimi is likely broken now due to new 7z encryption
11. [ ] review recent changes in onboard ui and see if there are issues
12. [x] come up with a plan to create proper CI/CD in our gitlab. put it in docs/proposals. Completed 2026-07-23: `docs/proposals/GITLAB_CI_CD_PLAN.md`.
13. [x] follow up from item 12. work on script or build pipeline for this. in its own directory in the repo. Completed 2026-07-23.
    <details>
    <summary><b>Completion and verification</b></summary>

    - Added root `.gitlab-ci.yml` as a small composition root, with workflow, defaults, validation, Rust, integration, and security jobs under `.gitlab/ci/`; reusable helpers and pinned Python test dependencies live under `.gitlab/scripts/` and `.gitlab/requirements/`.
    - Merge-request workflow suppresses duplicate branch pipelines. Required MR jobs are interruptible, unprivileged, secret-free, and assigned only to configurable `lint` and `rust-x86_64` runner tags.
    - Added blocking format/metadata, four-shape Cargo checks, workspace library tests, hermetic libSQL tests, Python suites, shell syntax/harnesses, registry JSON, version/panic policy, cargo-deny, release-tag consistency, and current-tree Gitleaks jobs. Known Clippy, architecture-boundary, ShellCheck, and historical-secret debt is explicitly observational with named removal conditions.
    - Pinned Rust 1.92.0, OCI image digests, cargo-deny 0.20.2 archive checksum, Gitleaks 8.30.1 image digest, and full Python test dependency versions. Cargo commands use `taskset -c 0-5` and `-j6`; Rust test cases run serially because existing config/setup tests mutate process-global environment state. No CI job uses a full debug `cargo build`.
    - Fixed baseline blockers: quality-gate working directories/resources, forge-neutral version inputs, panic scanner monorepo paths, invalid Python requirements syntax, malformed `ic/stuck.sh`, yanked `spin` 0.9.8, global test-env leakage, and missing `integration` gating for the prebuilt-WASM test target.
    - Local verification passed: all four Cargo check shapes; main and workspace library test suites; full libSQL test tier and doctests; 214 Python tests with no skips; fixture shell harnesses; cargo-deny advisories/bans/licenses/sources; redacted Gitleaks current-tree scan; CI YAML structure; Bash/ShellCheck for changed helpers; Python syntax; registry JSON; release-policy and panic-policy self-tests; `git diff --check`.
    - Added detailed GitLab CE activation runbook at `.gitlab/README.md`: isolated runner registration/config, tags and capacities, network policy, cache separation, protected branch/tag settings, protected-variable isolation, merge checks, CI Lint, first-MR proof, nightly schedule, and required activation evidence.
    - Authenticated GitLab CI Lint and one clean Rust 1.92 runner pipeline remain activation steps because this branch is not yet present on GitLab. PostgreSQL integration, strict WASM/browser, standalone crates, packages, releases, and deployment remain later phases from `docs/proposals/GITLAB_CI_CD_PLAN.md`.
    - Production CD remains intentionally absent. Current mt-admin upgrades rebuild per tenant and cannot yet activate the exact artifact tested by CI safely.
    </details>
14. [ ] review kawarimi adapter (item 15)
15. [ ] review darkirc key exchange implementation
16. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
17. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
18. [ ] update any ops docs in docs/ops that may contain outdated information. verify against code rather than docs
19. [ ] update any proposals docs in docs/proposals that may contain outdated information. verify against code rather than docs
20. [ ] update docs/README.md
21. [ ] update README.md at repo root
22. [ ] come up with codename ideas for release
23. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.2.0 explaining all relevant changes since v2.0.1.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is: `UNKNOWN` — The file you write will be RELEASE-v2.0.2.0.md and should be written to the ROOT of the repo
24. [ ] Improve accuracy of RELEASE-v2.0.2.0.md

---
