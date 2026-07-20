# LunarWing Documentation

All project documentation, organized by category. This index reflects the current tree —
every **active** file is listed below. Archived material lives under
[`internal/history/`](internal/history/) (superseded LunarWing docs).

* Original Author: Starforce Nebula *
* Updated on July 6th by Rarity to reflect recent changes to the documentation tree: added missing entries for reviews/, specs/, superpowers/; fixed broken links (ROADMAP_2026, DOCS_REORG_CHECKLIST); added READMEs to previously undocumentated subdirectories.*
* Updated on July 20th, 2026 to reconcile the complete proposal index with current source and tests at revision `50c8f99`.*

## Directory Structure

### 📐 [`architecture/`](architecture/)

System design and technical architecture documents (the authoritative specs).

| File | Description |
|------|-------------|
| [`ENGINE-V2.md`](architecture/ENGINE-V2.md) | V2 engine: threads, capabilities, CodeAct, gates, learning missions (linked from `CLAUDE.md`) |
| [`SSH_AGENT_HARNESS.md`](architecture/SSH_AGENT_HARNESS.md) | SSH agent harness: per-tenant in-process ssh-agent, encrypted key store, worker socket injection, host-key model |
| [`SSH_DELIVERY_MECHANISMS.md`](architecture/SSH_DELIVERY_MECHANISMS.md) | The three ways the agent runs SSH: worker mode, the `ssh`/`ssh_git` built-in tools, and the WASM `ssh` tool — when-to-use, security, enablement |
| [`SEMANTIC-MEMORY-SEARCH.md`](architecture/SEMANTIC-MEMORY-SEARCH.md) | Hybrid FTS + vector memory search, RRF fusion, embeddings (linked from `CLAUDE.md`) |
| [`WEECHAT-CHANNEL-ARCHITECTURE.md`](architecture/WEECHAT-CHANNEL-ARCHITECTURE.md) | WeeChat channel: components, message flow, ingestion/latency, config precedence, known issues |
| [`XMPP_FILE_TRANSFERS.md`](architecture/XMPP_FILE_TRANSFERS.md) | XMPP file transfer (XEP-0363/0066/0454): inbound/outbound, OMEMO, limits |
| [`SELF_HEAL_DEPLOYMENT_WIRING.md`](architecture/SELF_HEAL_DEPLOYMENT_WIRING.md) | How the infra health-check + self-heal pipeline is installed/scheduled (host-level, auto-scheduled by add-tenant as of v1.1.9) |
| [`ATOMICBOOL_DEEPER_PROPAGATION.md`](architecture/ATOMICBOOL_DEEPER_PROPAGATION.md) | Design note: deeper AtomicBool suppression-flag propagation (deferred) |

---

### 📖 [`guides/`](guides/)

How-to guides, build instructions, and setup walkthroughs.

| File | Description |
|------|-------------|
| [`MT-ADMIN-QUICKSTART.md`](guides/MT-ADMIN-QUICKSTART.md) | `lunarwing-mt-admin.sh` multi-tenant quickstart |
| [`CREATE_TENANT_NEW_SCRIPT.md`](guides/CREATE_TENANT_NEW_SCRIPT.md) | Creating a tenant with the new script |
| [`MIGRATE_IRONCLAW_TO_LUNARWING.md`](guides/MIGRATE_IRONCLAW_TO_LUNARWING.md) | Migrate an existing IronClaw PostgreSQL instance to LunarWing |
| [`MIGRATE_IRONCLAW_TO_MT.md`](guides/MIGRATE_IRONCLAW_TO_MT.md) | PostgreSQL migration to multi-tenant |
| [`MIGRATE_IRONCLAW_LIBSQL_TO_MT.md`](guides/MIGRATE_IRONCLAW_LIBSQL_TO_MT.md) | libSQL cross-backend migration to multi-tenant |
| [`TESTING_GUIDE.md`](guides/TESTING_GUIDE.md) | Pre-release test checklist + automated release testing (linked from `CLAUDE.md`) |
| [`EMBEDDINGS_SETUP.md`](guides/EMBEDDINGS_SETUP.md) | Embedding provider config (OpenAI-compatible, Ollama, NEAR AI) |
| [`VISION_OCR_SIDECAR.md`](guides/VISION_OCR_SIDECAR.md) | Vision/OCR sidecar service overview |
| [`GENTOO_PACKAGE_LIST.md`](guides/GENTOO_PACKAGE_LIST.md) | Gentoo package list + rootless-podman MT prerequisites |
| [`MULTICA_DEPLOYMENT.md`](guides/MULTICA_DEPLOYMENT.md) | Multica/Lunartica deployment guide |
| [`human-delay-mode.md`](guides/human-delay-mode.md) | Human-delay mode overview |
| [`DEBUG_LOG.md`](guides/DEBUG_LOG.md) | Catalog of debug/info log points by `file:line` |
| [`ENABLING_DEV_TOOLS.md`](guides/ENABLING_DEV_TOOLS.md) | Enabling filesystem/shell developer tools for a tenant |
| [`SSH-TOOL-TESTING.md`](guides/SSH-TOOL-TESTING.md) | Test prompts for validating the three SSH delivery mechanisms |
| [`AI-CODE-CONTRIBUTION-POLICY.md`](guides/AI-CODE-CONTRIBUTION-POLICY.md) | AI code contribution policy (effective July 7th, 2026) |
| [`CLAUDE_CODEX_ORCHESTRATION.md`](guides/CLAUDE_CODEX_ORCHESTRATION.md) | Claude Code → Codex parallel feature orchestration (worktrees, `/codex-impl`, `/features-parallel`) |
| [`darkirc_channel_for_lunarwing/BUILD_INSTRUCTIONS.md`](guides/darkirc_channel_for_lunarwing/BUILD_INSTRUCTIONS.md) | DarkIRC channel build instructions |
| [`darkirc_channel_for_lunarwing/DARKIRC_BUILD_GUIDE.md`](guides/darkirc_channel_for_lunarwing/DARKIRC_BUILD_GUIDE.md) | Full DarkIRC build guide |
| [`darkirc_channel_for_lunarwing/DARKIRC_MT_ADAPTER.md`](guides/darkirc_channel_for_lunarwing/DARKIRC_MT_ADAPTER.md) | DarkIRC multi-tenant adapter |
| [`lunarwing_weechat_wss/README.md`](guides/lunarwing_weechat_wss/README.md) | WeeChat WSS channel overview |
| [`lunarwing_weechat_wss/weechat_relay/INSTALL.md`](guides/lunarwing_weechat_wss/weechat_relay/INSTALL.md) | WeeChat relay installation |
| [`lunarwing_weechat_wss/weechat_relay/TROUBLESHOOTING.md`](guides/lunarwing_weechat_wss/weechat_relay/TROUBLESHOOTING.md) | WeeChat relay troubleshooting |
| [`gotify-wasm/README.md`](guides/gotify-wasm/README.md) | Gotify WASM tool |
| [`ic_sm/README.md`](guides/ic_sm/README.md) | Secret manager (`ic_sm`) |
| [`git-lunarwing-unix-socket-repl-server-repo/README.md`](guides/git-lunarwing-unix-socket-repl-server-repo/README.md) | REPLv2 Unix-socket REPL server |
| [`nanocode-config/README.md`](guides/nanocode-config/README.md) | Nanocode custom-config overview |

---

### 🛠️ [`ops/`](ops/)

Deployment, operations, multitenancy, and production guides.

| File | Description |
|------|-------------|
| [`MULTITENANCY-PRODUCTION.md`](ops/MULTITENANCY-PRODUCTION.md) | Production multi-tenancy walkthrough |
| [`DARKIRC-MULTITENANT.md`](ops/DARKIRC-MULTITENANT.md) | DarkIRC multitenant operations: port migration, provisioning, lifecycle, health, troubleshooting |
| [`DARKIRC-MULTITENANT-CHANGES-JUN23-FLAG-INFO.md`](ops/DARKIRC-MULTITENANT-CHANGES-JUN23-FLAG-INFO.md) | `--enable-darkirc` flag changes for mt-admin (June 2023 snapshot) |
| [`TENANT-CONFIGURATION.md`](ops/TENANT-CONFIGURATION.md) | Per-tenant configuration reference (env, LLM, XMPP, ports) |
| [`TENANT-RENAME-MIGRATION-1.1.9.md`](ops/TENANT-RENAME-MIGRATION-1.1.9.md) | Upgrading 1.1.7/1.1.8 tenants across the 1.1.9 directory renames (render-units flow, compat-symlink window, v2.0.0 deadline) |
| [`TEST-PLAN-UPGRADED-TENANT-1.1.9.md`](ops/TEST-PLAN-UPGRADED-TENANT-1.1.9.md) | Validated test plan for the upgraded-tenant path: synthesize a v1.1.8 tenant, upgrade, assert compat symlinks/protocol/render-units (OpenRC) |
| [`MULTITENANCY-HARNESS.md`](ops/MULTITENANCY-HARNESS.md) | Multi-tenant test harness guide |
| [`HARNESS-SINGLE-TENANT.md`](ops/HARNESS-SINGLE-TENANT.md) | Single-tenant test harness guide |
| [`SSH-HARNESS-SETUP.md`](ops/SSH-HARNESS-SETUP.md) | SSH harness setup & ops: config, key provisioning (mt-admin + manual), verifying in a worker, troubleshooting |
| [`WORKER-CONTAINERS.md`](ops/WORKER-CONTAINERS.md) | Worker container images (LunarWing, Nanocode, Pebble, Opencode) |
| [`PEBBLE-WORKER.md`](ops/PEBBLE-WORKER.md) | Pebble external worker operational guide |
| [`NANOCODE-MULTITENANT.md`](ops/NANOCODE-MULTITENANT.md) | Nanocode external worker, multi-tenant setup |
| [`WEECHAT-SERVICES.md`](ops/WEECHAT-SERVICES.md) | WeeChat services, ports, env vars, day-to-day ops |
| [`XMPP_KNOWN_ISSUES.md`](ops/XMPP_KNOWN_ISSUES.md) | XMPP/OMEMO known issues |
| [`XMPP_TRANSFERS.md`](ops/XMPP_TRANSFERS.md) | XMPP file-transfer methods quick-reference |
| [`RELEASE-COMMANDS.md`](ops/RELEASE-COMMANDS.md) | Release git/GitHub command template |
| [`RELEASE_CADENCE.md`](ops/RELEASE_CADENCE.md) | Release cadence policy |
| [`PRE-RELEASE-TESTING.md`](ops/PRE-RELEASE-TESTING.md) | Pre-release test status + test landscape |
| [`GOALS_1.1.9.md`](ops/GOALS_1.1.9.md) | Pre-release checklist for 1.1.9 (`Kiyome きよめ`) |
| [`ROADMAP_2026.MD`](ops/ROADMAP_2026.MD) | 2026 roadmap |
| [`MT-MACHINE-MIGRATION.md`](ops/MT-MACHINE-MIGRATION.md) | Migrating a multi-tenant host to a new machine |
| [`MT-GENTOO-SETUP-AND-CHANGES-MADE.md`](ops/MT-GENTOO-SETUP-AND-CHANGES-MADE.md) | Gentoo/OpenRC multi-tenant setup notes |
| [`guide-for-spin-up-gentoo-tenants.md`](ops/guide-for-spin-up-gentoo-tenants.md) | Step-by-step guide for spinning up a Gentoo/OpenRC tenant |
| [`MT-LEGACY-UPGRADE-NOTES.md`](ops/MT-LEGACY-UPGRADE-NOTES.md) | Legacy same-host version upgrade runbook (v1.0.3–v1.0.8 → v1.1.x) |
| [`SELF_REPAIR_IMPROVEMENTS_GENTOO.md`](ops/SELF_REPAIR_IMPROVEMENTS_GENTOO.md) | Self-heal fd-leak fix found on a live OpenRC MT host |
| [`ROADMAP_2026.md`](ops/ROADMAP_2026.md) | 2026 roadmap |
| [`GITHOOKS_HOW_TO.md`](ops/GITHOOKS_HOW_TO.md) | How to install githooks |
| [`IMPORT_EXPORT_EXAMPLE.txt`](ops/IMPORT_EXPORT_EXAMPLE.txt) | Worker import/export example |
| [`KUMOGAKURE_INSTRUCT.md`](ops/KUMOGAKURE_INSTRUCT.md) | Pointer to the external Notes vault |

Versioned release notes live in [`releases/`](releases/). Shipped per-release prep checklists are
archived in [`ops/history/`](ops/history/).

---

### 📋 [`reference/`](reference/)

Protocol specs, contract definitions, and API references.

| File | Description |
|------|-------------|
| [`custom_bridges/XMPP.md`](reference/custom_bridges/XMPP.md) | XMPP custom-bridge reference: architecture, loopback HTTP API, env vars, deployment |

---

### 📐 [`specs/`](specs/)

Standalone specifications for specific features or patterns.

| File | Description |
|------|-------------|
| [`podman-wait-babysitter.md`](specs/podman-wait-babysitter.md) | Podman wait-babysitter pattern spec |

---

### 🔍 [`reviews/`](reviews/)

Architecture and code reviews.

| File | Description |
|------|-------------|
| [`SWEETIE-ARCH-REVIEW.md`](reviews/SWEETIE-ARCH-REVIEW.md) | Full codebase architecture review by SweetieBot |
| [`SELF_IMPROVING_SKILLS_AUDIT_2026-07-15.md`](reviews/SELF_IMPROVING_SKILLS_AUDIT_2026-07-15.md) | Self-improving skills audit: B-1/B-2/B-3 tracks and Engine V2 feedback foundation, live E2E evidence, confirmed defects, corrected stale findings, P0/P1/P2 recommendations, default-disabled decision |
| [`ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md`](reviews/ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md) | Engine V2 channel parity audit for XMPP, DarkIRC, and WeeChat: routing, controls, statuses, proactive delivery, attachments, security findings, test evidence, and rollout decisions |

---

### [`plans/`](plans/)

Active implementation plans, execution backlogs, and delivery-status records.

| File | Description |
|------|-------------|
| [`ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md`](plans/ENGINE_V2_CHANNEL_PARITY_WORK_ITEMS_2026-07-18.md) | Prioritized implementation backlog derived from the Engine V2 XMPP/DarkIRC/WeeChat parity audit, with acceptance criteria, tests, dependencies, and rollout gates |
| [`e2e-skill-feedback.md`](plans/e2e-skill-feedback.md) | Live-tenant Engine V2 skill-feedback validation plan |
| [`SELF_IMPROVING_SKILLS_B2_B3.md`](plans/SELF_IMPROVING_SKILLS_B2_B3.md) | B-2/B-3 self-improving skills implementation plan and delivered status |
| [`WeeChat-Bootstrap.md`](plans/WeeChat-Bootstrap.md) | Secure tenant WeeChat relay provisioning plan |
| [`Near-Removal.md`](plans/Near-Removal.md) | Provider and WIT ABI naming migration plan |
| [`PHASE5PLANSTATUSTODAYWED.md`](plans/PHASE5PLANSTATUSTODAYWED.md) | Engine V2 local compatibility phase status record |
| [`SKILLb1b2b3STATUSTODAYWED.md`](plans/SKILLb1b2b3STATUSTODAYWED.md) | Self-improving skills implementation and verification status record |

---

### 🚀 [`superpowers/`](superpowers/)

Agent-driven plan and spec documents produced by the superpowers workflow.

| Subdirectory | Description |
|--------------|-------------|
| [`plans/`](superpowers/plans/) | Implementation plans (LunarVision VL wiring, vision-analyze tool, mt-admin SSH, mt-admin runtime persistence) |
| [`specs/`](superpowers/specs/) | Design specs corresponding to the plans |

---

### 💡 [`proposals/`](proposals/)

The directory contains active proposals plus retained implementation, verification,
superseded, and historical records. Every tracked file was audited against current
source and tests on 2026-07-20 at revision `50c8f99`; the dated status note in each
file is authoritative. Shipped or superseded records may later move to
[`internal/history/proposals/`](internal/history/proposals/), but remain indexed
here while they are still present in `proposals/`.

| File | Status | Description |
|------|--------|-------------|
| [`ADD-TENANTS-ADDITIONS.md`](proposals/ADD-TENANTS-ADDITIONS.md) | Historical | The proposed tenant flags shipped; guided setup now uses `lunarwing_mt_onboard` |
| [`AGENT_SSH_DEV_HARNESS.md`](proposals/AGENT_SSH_DEV_HARNESS.md) | Historical | Placeholder retained after the SSH harness shipped |
| [`APP_BUILDER_DIRECTION.md`](proposals/APP_BUILDER_DIRECTION.md) | Open | Unscoped App Builder enhancement idea |
| [`CARGO_TESTS_FIX.md`](proposals/CARGO_TESTS_FIX.md) | Partial | Ten test fixes landed; six architectural test cases remain deferred |
| [`CHAOS_FOLLOWUP_TESTS.md`](proposals/CHAOS_FOLLOWUP_TESTS.md) | Partial | Truncated-state coverage landed; most proposed chaos cases remain open |
| [`COOL_THINGS_THAT_HERMES_AGENT_HAS.md`](proposals/COOL_THINGS_THAT_HERMES_AGENT_HAS.md) | Historical | Roadmap snapshot with several shipped ideas and a larger open wishlist |
| [`DARKIRC_SECURE_KEY_EXCHANGE.md`](proposals/DARKIRC_SECURE_KEY_EXCHANGE.md) | Open | Secure contact-key exchange design; Phase 0 prerequisite is not implemented |
| [`DARKIRC_THINGS_TO_ADD.md`](proposals/DARKIRC_THINGS_TO_ADD.md) | Partial | Disabled-by-default behavior shipped; Tor and seed work remain open |
| [`ENGINE_LLM_STREAMING.md`](proposals/ENGINE_LLM_STREAMING.md) | Implemented | Engine streaming and cancellation phases 0-5; channel rollout validation remains operational |
| [`EXTERNAL-WORKER-AUDIT-2026-06-23.md`](proposals/EXTERNAL-WORKER-AUDIT-2026-06-23.md) | Implemented / historical | External-worker security findings and landed fixes |
| [`EXTERNAL-WORKER-PLAN-UPGRADES.md`](proposals/EXTERNAL-WORKER-PLAN-UPGRADES.md) | Implemented | Context, pooling, load balancing, and surviving worker updates landed |
| [`EXTERNAL-WORKER-UPGRADES-PROGRESS.md`](proposals/EXTERNAL-WORKER-UPGRADES-PROGRESS.md) | Implemented / historical | Completed external-worker progress record |
| [`GITWASM/README.md`](proposals/GITWASM/README.md) | Open | Gitoxide-based Git/Markdown WASM tool proposal |
| [`HTTP_TOOL_SSRF_PROTECTIONS.md`](proposals/HTTP_TOOL_SSRF_PROTECTIONS.md) | Implemented / historical | Investigation record for the active HTTP-tool SSRF controls |
| [`human-delay-mode-phase1-test-checklist.md`](proposals/human-delay-mode-phase1-test-checklist.md) | Implemented | Verified Phase 1 supervised-mode checklist |
| [`human-delay-mode-phase1-verification-handoff.md`](proposals/human-delay-mode-phase1-verification-handoff.md) | Superseded | Fulfilled Phase 1 verification handoff |
| [`human-delay-mode-phase2-plan.md`](proposals/human-delay-mode-phase2-plan.md) | Open | Timeout, gate-pipeline, modify-flow, and testing plan |
| [`human-delay-mode-testing-guide.md`](proposals/human-delay-mode-testing-guide.md) | Superseded | Stale guide with nonexistent standalone gate/thread CLI examples |
| [`IC_REPAIR_FOLLOWUPS.md`](proposals/IC_REPAIR_FOLLOWUPS.md) | Partial | Core self-heal fixes landed; escalation cooldown and deep tenant health remain |
| [`IRONCLAW_ADDITION_CANDIDATES.md`](proposals/IRONCLAW_ADDITION_CANDIDATES.md) | Open | Filtered current LunarWing port and hardening backlog |
| [`JINGLE_IBB_FEASIBILITY.md`](proposals/JINGLE_IBB_FEASIBILITY.md) | Historical | Deferred Jingle/IBB feasibility decision |
| [`KAWARIMI-OWNER-SCOPE-CONTINUITY.md`](proposals/KAWARIMI-OWNER-SCOPE-CONTINUITY.md) | Partial | Core import reconciliation landed; Phase 2 export/FK work remains |
| [`LOREBOOKS.md`](proposals/LOREBOOKS.md) | Open | Unscoped character-lorebook idea |
| [`LUNARVISION_POLISHING.md`](proposals/LUNARVISION_POLISHING.md) | Superseded | Health wiring and `vision_health` allocation landed elsewhere |
| [`MEMORY_MAINTENANCE_ROUTINES.md`](proposals/MEMORY_MAINTENANCE_ROUTINES.md) | Open | Routine-pack and reviewed memory-maintenance proposal/apply design |
| [`MT-1.1.0-TO-1.1.4-UPGRADE.md`](proposals/MT-1.1.0-TO-1.1.4-UPGRADE.md) | Implemented / verification pending | Rootless-flip tooling shipped; exact production flip remains unvalidated |
| [`MT-LEGACY-UPGRADE-CHERRYPICK-PLAN.md`](proposals/MT-LEGACY-UPGRADE-CHERRYPICK-PLAN.md) | Implemented / historical | Executed legacy-upgrade fix plan; production run remained operator-gated |
| [`MT-LEGACY-UPGRADE-QA-PLAN.md`](proposals/MT-LEGACY-UPGRADE-QA-PLAN.md) | Verification pending | Live legacy-upgrade operator checklist |
| [`MT-LEGACY-UPGRADE-VERIFICATION.md`](proposals/MT-LEGACY-UPGRADE-VERIFICATION.md) | Verification pending | Legacy harness canary, rollback, soak, and sign-off plan |
| [`MT-ONBOARDING-CLI.md`](proposals/MT-ONBOARDING-CLI.md) | Implemented | Shipped onboarding CLI, upgrade mode, and Kawarimi import/export extensions |
| [`MT-WEECHAT-CONSISTENCY-AND-CHANNEL-PRUNING.md`](proposals/MT-WEECHAT-CONSISTENCY-AND-CHANNEL-PRUNING.md) | Superseded | Historical deployment snapshot; allowlist and log rotation ideas remain |
| [`MULTICA_INTEGRATION_PLAN.md`](proposals/MULTICA_INTEGRATION_PLAN.md) | Partial | Bridge and polling channel shipped; real-time WS and skill import remain open |
| [`MULTICA_LUNARTICA_RESKIN.md`](proposals/MULTICA_LUNARTICA_RESKIN.md) | Open / external | Lunartica UI reskin plan for a separate repository |
| [`MULTICA_SUPPORT.md`](proposals/MULTICA_SUPPORT.md) | Superseded | Stub absorbed by the main Multica integration plan |
| [`OH-MY-OPENAGENT.md`](proposals/OH-MY-OPENAGENT.md) | Partial | Plugin bundled in managed configs; no standalone drop-in config exists |
| [`OLDPROJECT_PORT_ANALYSES/README.md`](proposals/OLDPROJECT_PORT_ANALYSES/README.md) | Partial / verification pending | Status index for the five pre-fork analyses; advisory baseline needs refresh |
| [`OLDPROJECT_PORT_ANALYSES/ironclaw-0.28.1-port-analysis.md`](proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-0.28.1-port-analysis.md) | Partial | Approval clamping landed; remaining selected ports stay open |
| [`OLDPROJECT_PORT_ANALYSES/ironclaw-0.28.2-port-analysis.md`](proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-0.28.2-port-analysis.md) | Partial | Security fixes landed; model facade and snapshot harness remain open |
| [`OLDPROJECT_PORT_ANALYSES/ironclaw-0.29.0-port-analysis.md`](proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-0.29.0-port-analysis.md) | Partial / stale | Logs backend landed; Wasmtime/advisory snapshot requires a fresh audit |
| [`OLDPROJECT_PORT_ANALYSES/ironclaw-0.29.1-port-analysis.md`](proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-0.29.1-port-analysis.md) | Implemented / historical | Non-UUID conversation isolation port record |
| [`OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md`](proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md) | Open | Selective Reborn mechanism backlog; no wholesale adoption |
| [`OPENCODE-WORKER-SINGLE-TARGET-BUILD.md`](proposals/OPENCODE-WORKER-SINGLE-TARGET-BUILD.md) | Implemented | OpenCode worker now builds only its native target |
| [`OPENRC_ACCURATE_REPORT_16_JUNE_2026.md`](proposals/OPENRC_ACCURATE_REPORT_16_JUNE_2026.md) | Partial / historical | Curative fixes landed; broader OpenRC hardening remains open |
| [`PASEO.MD`](proposals/PASEO.MD) | Open | Unscoped Paseo support link stub |
| [`PAST.MD`](proposals/PAST.MD) | Open | Unscoped PAST+SS streaming idea |
| [`PER_TENANT_WORKER_GATING.md`](proposals/PER_TENANT_WORKER_GATING.md) | Implemented | Per-tenant worker selection persistence and start gating landed |
| [`PODMAN_WAIT_BABYSITTER.md`](proposals/PODMAN_WAIT_BABYSITTER.md) | Implemented | OpenRC/rootless Podman babysitter pattern as built |
| [`PODMAN_WAIT_BABYSITTER_REVIEW.md`](proposals/PODMAN_WAIT_BABYSITTER_REVIEW.md) | Historical | Review findings resolved by the landed babysitter implementation |
| [`ports_expansion.txt`](proposals/ports_expansion.txt) | Partial | `vision_health` landed; future range/proxy decision remains open |
| [`PR_REVIEW_23_24.md`](proposals/PR_REVIEW_23_24.md) | Historical | Point-in-time review of two unmerged PRs |
| [`Profiles.md`](proposals/Profiles.md) | Open | Unscoped agent-profile/personality idea |
| [`qwen3vl-ocr-podman.md`](proposals/qwen3vl-ocr-podman.md) | Open / external | External Qwen3-VL and Tesseract Podman runbook |
| [`REFINE_LIBSQL_MIGRATION_GUIDE.md`](proposals/REFINE_LIBSQL_MIGRATION_GUIDE.md) | Open | Runtime-agnostic libSQL migration-guide refinement |
| [`RENDER_UNITS_SMALL_BUG.md`](proposals/RENDER_UNITS_SMALL_BUG.md) | Partial | `render-units` fix landed; optional-WeeChat health footgun remains |
| [`rootless-podman-babysitter.md`](proposals/rootless-podman-babysitter.md) | Implemented | Completed and reviewed rootless-Podman babysitter plan |
| [`ROOTLESS_PODMAN_CONTAINER_SUPERVISION_GAP.md`](proposals/ROOTLESS_PODMAN_CONTAINER_SUPERVISION_GAP.md) | Superseded | Gap closed by OpenRC babysitters and systemd Quadlets |
| [`ROUTINE_ENGINE_IMPROVEMENTS.md`](proposals/ROUTINE_ENGINE_IMPROVEMENTS.md) | Partial | Items 1-3 landed; items 4-8 remain deprioritized |
| [`SELF_IMPROVING_SKILLS_B1.md`](proposals/SELF_IMPROVING_SKILLS_B1.md) | Implemented | End-to-end reviewed skill-patching proposal flow |
| [`SESSION-AUDIT-MT-DARKIRC-EWE-2026-06-23.md`](proposals/SESSION-AUDIT-MT-DARKIRC-EWE-2026-06-23.md) | Partial | Major fixes landed; remaining tracker items stay open |
| [`SSH_HARNESS_DELIVERY_OPTIONS.md`](proposals/SSH_HARNESS_DELIVERY_OPTIONS.md) | Implemented | All three SSH delivery mechanisms shipped |
| [`SSH_HARNESS_OPTION_2_3_IMPLEMENTATION.md`](proposals/SSH_HARNESS_OPTION_2_3_IMPLEMENTATION.md) | Implemented / historical | Pre-implementation design record for shipped options 2 and 3 |
| [`SUPERGATEWAY_MCP.md`](proposals/SUPERGATEWAY_MCP.md) | Superseded | Native stdio MCP support made the workaround optional |
| [`TEST_IN_PLACE_UPGRADE_IN_NEW_WEB_GUI.md`](proposals/TEST_IN_PLACE_UPGRADE_IN_NEW_WEB_GUI.md) | Open stub | Placeholder without a test plan |
| [`TEST_KAWARIMI_IMPORT_AND_EXPORT_IN_NEW_WEB_GUI.md`](proposals/TEST_KAWARIMI_IMPORT_AND_EXPORT_IN_NEW_WEB_GUI.md) | Open stub | Placeholder without web-GUI acceptance criteria |
| [`UPGRADE_AND_MIGRATION_ISSUES_TO_FIX.md`](proposals/UPGRADE_AND_MIGRATION_ISSUES_TO_FIX.md) | Partial / mostly superseded | Owner-scope risk closed; remaining operational cautions retained |
| [`V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md`](proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md) | Partial / exploratory | Phase 0 stdio install landed; broader V2 package/runtime work remains |
| [`WEECHAT_CLIENT_RELAY_API_AUTOMATION.md`](proposals/WEECHAT_CLIENT_RELAY_API_AUTOMATION.md) | Partial | Relay bootstrap shipped; full client provisioning remains open |
| [`WEECHAT_WS_ADAPTER_MISSING_DEPENDENCY_AND_AUTOMATION.md`](proposals/WEECHAT_WS_ADAPTER_MISSING_DEPENDENCY_AND_AUTOMATION.md) | Partial | Service and port automation landed; dependency install remains warn-only |
| [`WEECHAT_WS_ADAPTER_SYNC_PROTOCOL.md`](proposals/WEECHAT_WS_ADAPTER_SYNC_PROTOCOL.md) | Superseded | `ironclaw-sync` was renamed to `lunarwing-sync` |
| [`WORK_ON_LUNAR_SPRITE.md`](proposals/WORK_ON_LUNAR_SPRITE.md) | Open stub | Placeholder without a proposal or implementation |
| [`XMPP_INCOMING_ATTACHMENT.md`](proposals/XMPP_INCOMING_ATTACHMENT.md) | Superseded | Stale compiler-warning capture; helper is test-gated and used |
| [`XMPP_LUNARVISION_INTEGRATION.md`](proposals/XMPP_LUNARVISION_INTEGRATION.md) | Open | Automatic XMPP image routing into LunarVision remains unimplemented |
| [`XMPP_OMEMO_AESGCM_URL_LEAK_FIX.md`](proposals/XMPP_OMEMO_AESGCM_URL_LEAK_FIX.md) | Implemented | Embedded and URL-only `aesgcm://` stripping regressions are present |
| [`XMPP_WASM_ATTACHMENT_TRAP.md`](proposals/XMPP_WASM_ATTACHMENT_TRAP.md) | Partial / verification pending | Memory/decode fixes landed; large-attachment regression and live PNG gate remain |

---

### 🔍 [`reviews/`](reviews/)

Third-party and internal architecture reviews of the LunarWing codebase.

| File | Description |
|------|-------------|
| [`SWEETIE-ARCH-REVIEW.md`](reviews/SWEETIE-ARCH-REVIEW.md) | Full codebase architecture review by SweetieBot (system overview, agent loop, session/thread model, tool system, channels, orchestration, workspace/memory) |
| [`SELF_IMPROVING_SKILLS_AUDIT_2026-07-15.md`](reviews/SELF_IMPROVING_SKILLS_AUDIT_2026-07-15.md) | Self-improving skills audit: B-1/B-2/B-3 tracks and Engine V2 feedback foundation, live E2E evidence, confirmed defects, corrected stale findings, P0/P1/P2 recommendations, default-disabled decision |
| [`ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md`](reviews/ENGINE_V2_CHANNEL_PARITY_AUDIT_2026-07-18.md) | Engine V2 channel parity audit for XMPP, DarkIRC, and WeeChat: routing, controls, statuses, proactive delivery, attachments, security findings, test evidence, and rollout decisions |

---

### 📐 [`specs/`](specs/)

Technical specifications for specific subsystems and patterns.

| File | Description |
|------|-------------|
| [`podman-wait-babysitter.md`](specs/podman-wait-babysitter.md) | Podman wait babysitter pattern: OpenRC crash recovery for rootless Podman containers via `supervise-daemon` |

---

### 🦸 [`superpowers/`](superpowers/)

Dated implementation plans and design specs produced by the superpowers agentic workflow.

#### Plans

| File | Description |
|------|-------------|
| [`plans/2026-06-29-lunarvision-vl-wiring.md`](superpowers/plans/2026-06-29-lunarvision-vl-wiring.md) | LunarVision VL wiring: connect GPU-backed Qwen3-VL server to OCR sidecars via `VL_URL`, persist as systemd unit |
| [`plans/2026-06-30-vision-analyze-tool-wiring.md`](superpowers/plans/2026-06-30-vision-analyze-tool-wiring.md) | Vision-analyze WASM tool wiring: re-register with WASM toolset, end-to-end integration |
| [`plans/2026-07-01-mt-admin-ssh-streamlining.md`](superpowers/plans/2026-07-01-mt-admin-ssh-streamlining.md) | `mt-admin` SSH streamlining: key upload, daemon bounce, worker startup ordering |
| [`plans/2026-07-02-mt-admin-runtime-persistence.md`](superpowers/plans/2026-07-02-mt-admin-runtime-persistence.md) | `mt-admin` runtime persistence: state file management, idempotent operations |
| [`plans/2026-07-10-cross-version-diverged-branch-integration.md`](superpowers/plans/2026-07-10-cross-version-diverged-branch-integration.md) | Corrected cross-version port of serde, Codex-provider removal, lockfile, CLI, and proposal changes |

#### Design Specs

| File | Description |
|------|-------------|
| [`specs/2026-06-29-lunarvision-vl-wiring-design.md`](superpowers/specs/2026-06-29-lunarvision-vl-wiring-design.md) | Design spec for LunarVision VL wiring |
| [`specs/2026-06-30-vision-analyze-tool-wiring-design.md`](superpowers/specs/2026-06-30-vision-analyze-tool-wiring-design.md) | Design spec for vision-analyze tool wiring |
| [`specs/2026-07-01-mt-admin-ssh-streamlining-design.md`](superpowers/specs/2026-07-01-mt-admin-ssh-streamlining-design.md) | Design spec for mt-admin SSH streamlining |
| [`specs/2026-07-02-mt-admin-runtime-persistence-design.md`](superpowers/specs/2026-07-02-mt-admin-runtime-persistence-design.md) | Design spec for mt-admin runtime persistence |
| [`specs/2026-07-10-cross-version-diverged-branch-integration-design.md`](superpowers/specs/2026-07-10-cross-version-diverged-branch-integration-design.md) | Design spec for corrected branch integration across LunarWing V1 and V2 |

---

### 📦 [`releases/`](releases/)

Release notes and changelogs (immutable historical records). See [`releases/README.md`](releases/README.md) for the full index (v1.0.7 → v1.1.8, latest: RELEASE-v1.1.8.md).

---

### 🐛 [`bugs/`](bugs/)

A maintained Open/Fixed bug tracker. The full index is **[`bugs/README.md`](bugs/README.md)**.
Open bugs live in `bugs/`; resolved bugs are retained under [`bugs/history/`](bugs/history/).
A maintained Open/Fixed bug tracker. The full index is **[`bugs/README.md`](bugs/README.md)** —
it lists bug docs (Open + Fixed-retained-for-history) plus the two 1.1.4 multi-tenant
pre-release issue logs (`SYSTEMD-MT-1.1.4-ISSUES.md`, `OPENRC-MT-1.1.4-ISSUES.md`).

---

### 🗂️ [`internal/`](internal/)

Internal notes, drafts, and working documents — incomplete or in-progress by nature.

| File | Description |
|------|-------------|
| [`FORK_CONTEXT.md`](internal/FORK_CONTEXT.md) | Fork history and context (authoritative; linked from `CLAUDE.md`) |
| [`.github/pull_request_template.md`](internal/.github/pull_request_template.md) | LunarWing PR template (review tracks, validation checklist) |
| [`CHANGELOG-AGENTS.md`](internal/CHANGELOG-AGENTS.md) | Internal changelog of `AGENTS.md` updates |
| [`ic-infrastructure-health-check/draft-ic-infrastructure-health-check-analysis-report.md`](internal/ic-infrastructure-health-check/draft-ic-infrastructure-health-check-analysis-report.md) | Health-check analysis draft |
| [`nanocode-config/KAGEHO_QUESTIONS.md`](internal/nanocode-config/KAGEHO_QUESTIONS.md) | Agnostic coding-worker container design notes |
| [`nanocode-config/NextSteps.md`](internal/nanocode-config/NextSteps.md) | Nanocode + TensorZero setup next steps |
| [`COMPONENT_SOURCES.md`](internal/COMPONENT_SOURCES.md) | Where each custom component's upstream source repo lives |

**Archives** (kept for provenance, not active docs):

- [`history/`](internal/history/) — superseded LunarWing docs relocated here during the reorg, by source area: `architecture/`, `guides/`, `internal/`, `proposals/`. See [`history/README.md`](internal/history/README.md).

---

### 📝 Top-level docs

There are currently no top-level files in `docs/` outside the subdirectories above.
- [`history/`](internal/history/) — superseded LunarWing docs relocated here during the reorg, by source area: `architecture/`, `guides/`, `internal/`, `proposals/`, `archive/`. See [`history/README.md`](internal/history/README.md).

---

## What Stays Outside `docs/`

The following are **not** in this directory and should remain where they are:

- **`README.md`** — repo root entry point
- **`CLAUDE.md` / `AGENTS.md` / `CODEX.md`** — AI agent context files (kept at their respective locations)
- **`ic/`** — all documentation within the `ic/` tree stays in place (workspace templates, crate docs, skill definitions, etc.)
- **`projects/`** — satellite service documentation stays with its source (e.g., `projects/ocr-sidecar/README.md`)
- **`lunarcode4lunarwing/` / `pebble4lunarwing/` / `opencode4lunarwing/`** — worker-container docs stay with their container source
- **`.claude/`** — Claude command and rule files

---

## Contributing

When adding new documentation, place it in the appropriate subdirectory:

1. **`architecture/`** — if it describes *why* the system is designed a certain way
2. **`guides/`** — if it tells someone *how to do* something
3. **`ops/`** — if it covers *deployment or operations*
4. **`reference/`** — if it's a *protocol spec, contract, or API doc*
5. **`specs/`** — if it's a *standalone feature specification*
6. **`plans/`** — if it's an *implementation plan, execution backlog, or delivery-status record*
7. **`proposals/`** — if it's a *proposal or design for not-yet-shipped work*
8. **`bugs/`** — if it's a *bug report* (and add it to [`bugs/README.md`](bugs/README.md))
9. **`reviews/`** — if it's a *codebase or architecture review*
10. **`internal/`** — if it's a *draft, note, or working document*

Superseded or shipped docs are archived under `internal/history/` (LunarWing-authored). Update this index when adding or moving files.
