# The Roadmap Document

LunarWing 2026 release roadmap. Each entry lists its target release; entries
backed by a design or proposal document cite the source so the plan stays
traceable. Proposals whose work is already represented below are not
duplicated; open/unscoped stubs without a design are parked at the bottom.

things to add

Add

### Container Registry

### update opencode to new version

### add kimicode harness! — DONE v2.0.3.0

| Feature | Target | Source |
|---------|--------|--------|
| Lorebook support / Agent Profile enhancements / Workspace Seeding improvements / Agent Profile switching / User Profile switching (further planning required) | v2.0.4 | `docs/proposals/LOREBOOKS.md`, `docs/proposals/Profiles.md` |
| DarkIRC key exchange automation | v2.0.4 | `docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md` |
| Self-Healing Capabilities analysis of any missing pieces from all the new components. Implementation of missing pieces to follow | v2.0.4 | `docs/proposals/CHAOS_FOLLOWUP_TESTS.md`, `docs/proposals/IC_REPAIR_FOLLOWUPS.md` |
| More Crate Updates | v2.0.4 | |
| MCP Overhaul Part 1 | v2.0.4 | `docs/proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md` |
| Reflex compiler improvements | v2.0.4 | `docs/proposals/REFLEX_COMPILER_V2_UPGRADE_PLAN.md`, `docs/proposals/REFLEXCOMPILERANDSOFTWAREBUILDERUPDATESNEEDED/` |
| Multi-agent agent team orchestration via XMPP | v2.0.4 | `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md` |
| IronClaw port backlog — top 3 easiest additions | v2.0.4 | `docs/proposals/IRONCLAW_TOP3_EASIEST_PLAN.md`, `docs/proposals/OLDPROJECT_PORT_ANALYSES/` |
| GitLab CI/CD activation and Phase 2 | v2.0.4 | `docs/proposals/GITLAB_CI_CD_PLAN.md` |
| Memory maintenance routines | v2.0.4 | `docs/proposals/MEMORY_MAINTENANCE_ROUTINES.md` |
| Kawarimi owner-scope continuity — remaining phases | v2.0.4 | `docs/proposals/KAWARIMI-OWNER-SCOPE-CONTINUITY.md` |
| Per-tenant WeeChat health-glob gate (fix the flap /`render-units` footgun) | v2.0.5 | `docs/proposals/RENDER_UNITS_SMALL_BUG.md` |
| Security hardening | v2.0.5 | |
| Deprecate Nanocode external Worker | v2.0.5 | |
| Lunartica UI reskin continuation | v2.0.5 | `docs/proposals/MULTICA_LUNARTICA_RESKIN.md` |
| Upgrade old testing harness | v2.0.5 | |
| Further external worker polishing | v2.0.5 | `docs/proposals/EXTERNAL-WORKER-PLAN-UPGRADES.md` |
| Feature set of subset of agent features adopted from Hermes Agent (Human Delay mode already landed in a prior release); comprehensive documentation to accompany each new feature | v2.0.6 | `docs/proposals/COOL_THINGS_THAT_HERMES_AGENT_HAS.md` |
| Human Delay Mode Phase 2 | v2.0.6 | `docs/proposals/human-delay-mode-phase2-plan.md` |
| Input-validation security improvements | v2.0.6 | |
| ONNX Runtime speech to text model support necessary for LunarVoice | v2.0.6 | |
| MCP Overhaul Part 2 | v2.0.6 | `docs/proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md` |
| IronClaw port backlog — top 2 priority additions | v2.0.6 | `docs/proposals/IRONCLAW_TOP2_PRIORITY_PLAN.md` |
| Routine engine improvements — remaining items 4-8 | v2.0.6 | `docs/proposals/ROUTINE_ENGINE_IMPROVEMENTS.md` |
| DarkIRC decentralized peering — Tor transport for onion seeds and expanded clearnet seed availability | v2.0.6 | `docs/proposals/DARKIRC_THINGS_TO_ADD.md` |
| WeeChat client relay API automation — remaining automation and adapter dependency work | v2.0.6 | `docs/proposals/WEECHAT_CLIENT_RELAY_API_AUTOMATION.md`, `docs/proposals/WEECHAT_WS_ADAPTER_MISSING_DEPENDENCY_AND_AUTOMATION.md` |
| XMPP WASM attachment trap resolution | v2.0.6 | `docs/proposals/XMPP_WASM_ATTACHMENT_TRAP.md` |
| Additional Opencode External Worker Polishing | v2.0.7 | |
| Reflex Compiler polishing and improvements | v2.0.7 | `docs/proposals/REFLEX_COMPILER_V2_UPGRADE_PLAN.md` |
| LunarVoice Two Way Voice Communication | v2.0.8 | |
| MCP Overhaul Part 3 | v2.0.8 | `docs/proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md` |
| App Builder (enhanced) direction | v2.0.8 | `docs/proposals/APP_BUILDER_DIRECTION.md` |
| Git WASM tool (Gitoxide-based) | v2.0.8 | `docs/proposals/GITWASM/` |
| Multica integration plan — remaining phases | v2.0.8 | `docs/proposals/MULTICA_INTEGRATION_PLAN.md` |
| oh-my-openagent config equivalence for OpenCode worker | v2.0.8 | `docs/proposals/OH-MY-OPENAGENT.md` |
| libSQL migration guide refinement | v2.0.8 | `docs/proposals/REFINE_LIBSQL_MIGRATION_GUIDE.md` |
| LunarVision OCR rootless-podman runbook | v2.0.8 | `docs/proposals/qwen3vl-ocr-podman.md` |
| Lunartica improvements | v2.0.9 | |
| Additional LunarVoice polishing | v2.0.9 | |
| A surprise | v2.1.0 | |

### Unscoped ideas (no release target yet)

Documented stubs that have no design or scope yet; promote them into a
release row once a real proposal exists:

- Lunar sprite work — `docs/proposals/WORK_ON_LUNAR_SPRITE.md`
- In-place upgrade testing in the new web GUI — `docs/proposals/TEST_IN_PLACE_UPGRADE_IN_NEW_WEB_GUI.md`
- Kawarimi import/export testing in the new web GUI — `docs/proposals/TEST_KAWARIMI_IMPORT_AND_EXPORT_IN_NEW_WEB_GUI.md`
- Paseo support — `docs/proposals/PASEO.MD`
- PAST+SS secure streaming investigation — `docs/proposals/PAST.MD`
- Port-range expansion follow-ups (SSH agent / reverse-proxy decision) — `docs/proposals/ports_expansion.txt`

---
