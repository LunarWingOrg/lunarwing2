# Proposals Overview

Simple listing of documents in `docs/proposals/`.

## Top-Level Files

| File | Brief Description |
|------|-------------------|
| ADD-TENANTS-ADDITIONS.md | Notes on additional tenant features |
| AGENT_SSH_DEV_HARNESS.md | SSH dev harness for agent work |
| APP_BUILDER_DIRECTION.md | Direction notes for AppBuilder architecture |
| CARGO_TESTS_FIX.md | Fix notes for cargo test issues |
| CHAOS_FOLLOWUP_TESTS.md | Follow-up tests for chaos testing |
| COOL_THINGS_THAT_HERMES_AGENT_HAS.md | Feature inventory from Hermes agent |
| DARKIRC_SECURE_KEY_EXCHANGE.md | Secure key exchange proposal for DarkIRC |
| DARKIRC_THINGS_TO_ADD.md | Additional DarkIRC features wishlist |
| ENGINE_LLM_STREAMING.md | LLM streaming design for the engine |
| EXTERNAL-WORKER-AUDIT-2026-06-23.md | Audit of external worker system |
| EXTERNAL-WORKER-PLAN-UPGRADES.md | Upgrade plan for external workers |
| EXTERNAL-WORKER-UPGRADES-PROGRESS.md | Progress tracking for external worker upgrades |
| HERMES-KAWARIMI-ADAPTER.md | Migration adapter from Hermes to LunarWing |
| HTTP_TOOL_SSRF_PROTECTIONS.md | SSRF protection proposals for HTTP tool |
| human-delay-mode-phase1-test-checklist.md | Phase 1 test checklist for human-delay mode |
| human-delay-mode-phase1-verification-handoff.md | Phase 1 verification handoff for human-delay mode |
| human-delay-mode-phase2-plan.md | Phase 2 plan for human-delay mode |
| human-delay-mode-testing-guide.md | Testing guide for human-delay mode |
| IC_REPAIR_FOLLOWUPS.md | Follow-up repair items for the IC daemon |
| IRONCLAW_ADDITION_CANDIDATES.md | Candidate additions from IronClaw analysis |
| IRONCLAW_TOP2_PRIORITY_PLAN.md | Plan for top 2 highest-priority IronClaw additions |
| IRONCLAW_TOP3_EASIEST_PLAN.md | Plan for top 3 easiest IronClaw additions |
| JINGLE_IBB_FEASIBILITY.md | Feasibility study for Jingle IBB support |
| KAWARIMI-OWNER-SCOPE-CONTINUITY.md | Owner scope continuity for the Kawarimi adapter |
| LOREBOOKS.md | Lorebook feature notes |
| LUNARVISION_POLISHING.md | Polishing items for LunarVision OCR |
| MEMORY_MAINTENANCE_ROUTINES.md | Maintenance routines for persistent memory |
| MT-1.1.0-TO-1.1.4-UPGRADE.md | Multi-tenant upgrade path 1.1.0 to 1.1.4 |
| MT-ADMIN-DECOMPOSITION.md | Decomposition plan for mt-admin script |
| MT-LEGACY-UPGRADE-CHERRYPICK-PLAN.md | Cherry-pick plan for legacy MT upgrades |
| MT-LEGACY-UPGRADE-QA-PLAN.md | QA plan for legacy MT upgrades |
| MT-LEGACY-UPGRADE-VERIFICATION.md | Verification notes for legacy MT upgrades |
| MT-ONBOARDING-CLI.md | CLI-based multi-tenant onboarding proposal |
| MT-WEECHAT-CONSISTENCY-AND-CHANNEL-PRUNING.md | Weechat consistency and channel pruning for MT |
| MULTICA_INTEGRATION_PLAN.md | Integration plan for MultiCA |
| MULTICA_LUNARTICA_RESKIN.md | LunarTica reskin for MultiCA |
| MULTICA_SUPPORT.md | MultiCA support notes |
| OH-MY-OPENAGENT.md | OpenAgent shell environment notes |
| OPENCODE-WORKER-SINGLE-TARGET-BUILD.md | Single-target build for OpenCode worker |
| OPENRC_ACCURATE_REPORT_16_JUNE_2026.md | OpenRC status report |
| PASEO.MD | Paseo-related notes |
| PAST.MD | Historical reference notes |
| PER_TENANT_WORKER_GATING.md | Per-tenant worker gating proposal |
| PODMAN_WAIT_BABYSITTER.md | Podman wait babysitter proposal |
| PODMAN_WAIT_BABYSITTER_REVIEW.md | Review of podman wait babysitter |
| ports_expansion.txt | Port expansion notes |
| Profiles.md | Profile configuration notes |
| PR_REVIEW_23_24.md | PR review notes |
| qwen3vl-ocr-podman.md | Qwen3VL OCR podman setup |
| REFINE_LIBSQL_MIGRATION_GUIDE.md | Refinement notes for libSQL migration guide |
| RENDER_UNITS_SMALL_BUG.md | Small bug report for render units |
| rootless-podman-babysitter.md | Rootless podman babysitter notes |
| ROOTLESS_PODMAN_CONTAINER_SUPERVISION_GAP.md | Supervision gap for rootless podman containers |
| ROUTINE_ENGINE_IMPROVEMENTS.md | Improvements for the routine engine |
| SELF_IMPROVING_SKILLS_B1.md | Self-improving skills proposal |
| SESSION-AUDIT-MT-DARKIRC-EWE-2026-06-23.md | Session audit for MT DarkIRC EWE |
| SSH_HARNESS_DELIVERY_OPTIONS.md | Delivery options for SSH harness |
| SSH_HARNESS_OPTION_2_3_IMPLEMENTATION.md | Implementation of SSH harness options 2 & 3 |
| SUPERGATEWAY_MCP.md | Supergateway MCP notes |
| TEST_IN_PLACE_UPGRADE_IN_NEW_WEB_GUI.md | Test plan for in-place upgrade in web GUI |
| TEST_KAWARIMI_IMPORT_AND_EXPORT_IN_NEW_WEB_GUI.md | Test plan for Kawarimi import/export in web GUI |
| UPGRADE_AND_MIGRATION_ISSUES_TO_FIX.md | Upgrade and migration issues to address |
| V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md | V2 MCP extension architecture exploration |
| WEECHAT_CLIENT_RELAY_API_AUTOMATION.md | Weechat client relay API automation |
| WEECHAT_WS_ADAPTER_MISSING_DEPENDENCY_AND_AUTOMATION.md | Missing dependency and automation for weechat WS adapter |
| WEECHAT_WS_ADAPTER_SYNC_PROTOCOL.md | Sync protocol for weechat WS adapter |
| WORK_ON_LUNAR_SPRITE.md | LunarSprite work notes |
| XMPP_INCOMING_ATTACHMENT.md | XMPP incoming attachment handling |
| XMPP_LUNARVISION_INTEGRATION.md | XMPP LunarVision integration |
| XMPP_OMEMO_AESGCM_URL_LEAK_FIX.md | OMEMO AES-GCM URL leak fix for XMPP |
| XMPP_WASM_ATTACHMENT_TRAP.md | WASM attachment trap for XMPP |

## Subdirectories

### `GITWASM/`
- `README.md` — GitWASM extension overview

### `OLDPROJECT_PORT_ANALYSES/`
- `README.md` — Overview of port analyses
- `ironclaw-0.28.1-port-analysis.md`
- `ironclaw-0.28.2-port-analysis.md`
- `ironclaw-0.29.0-port-analysis.md`
- `ironclaw-0.29.1-port-analysis.md`
- `ironclaw-1.0.0-rc.1-port-analysis.md`
- `ironclaw-reborn-port-analysis.md`
