# PRE-RELEASE CHECKLIST for LunarWing v2.0.9.0

## Codename: Unknown

**Open TODOs (v2.0.9.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)

### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] do another pass on the README.md at root of repo - update if necessary — DONE 2026-08-23 (rarity/kimi-1-20260823-0303): full verification pass; no changes necessary. All ~30 local links/assets resolve (docs index, guides, ops, releases, branding image, onboarding run.sh, mt-admin path). Spot-checked claims against code: toolchain 1.96.1/edition 2024 (ic/rust-toolchain.toml, workspace v2.0.9); Engine V2 + `ENGINE_V2_CHANNELS=xmpp,weechat,darkirc` default for new tenants (mt-admin-lib/06-env-generation.sh:670-672); health checks and SSH on by default with `--no-health`/`--no-ssh` opt-outs; MCP negotiation offers `2024-11-05` and accepts `2025-03-26` (ic/src/tools/mcp/protocol.rs:77-85, client.rs:1467-1481); seven canonical native study-notes tools (ic/src/workspace/README.md:96+); onboarding GUI port 1969 and five workflows (provision/secrets/upgrade/export/import endpoints in lunarwing_mt_onboard_web/app.py); Python 3.10+ requirement matches both onboarding READMEs; worker flags `--with-pebble/--with-opencode/--with-kimi`, `--with-study-mcp`, LOREBOOK_ENABLED, SKILL_SELF_IMPROVEMENT, Multica channel, XEP-0363 all present; cargo 0-11/-j12 limits still match AGENTS.md.
2. [x] do pass through of all README.md docs in docs/ specifically
3. [ ] investigate work item and see if issue still persists and fix if necessary: https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/55
4. [x] see if this work item has been addressed and implement if possible https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/47 — DONE 2026-08-23 (aj/kimi-4-20260823-0601): already addressed 100%; no new code needed. Work item #47 (global `tool_activity` flag routing `StatusUpdate::{ToolStarted,ToolCompleted,ToolResult}` to WASM channels) was implemented in v2.0.8.0 (AGENT_GOALS_2.0.8.0 item 17, commit 5a033716) and all pieces are confirmed present on this branch: host flag (`ic/src/settings.rs:501`, `ic/src/config/channels.rs:32` + `TOOL_ACTIVITY` env), gated routing in `ic/src/channels/wasm/wrapper.rs:2089` (`is_tool_activity_status`), threading through loader/setup/hot-activation (`wasm/loader.rs`, `wasm/setup.rs:54`, `extensions/manager.rs:719`, `main.rs:1005`), guest rendering in all three named channels (`channels-src/xmpp/src/lib.rs:472`, `weechat/src/lib.rs:1492`, `darkirc/src/lib.rs:482`; multica intentionally untouched per original note), and docs (RELEASE-v2.0.8.0.md, docs/ops/TENANT-CONFIGURATION.md:261). Verified: host `cargo test --lib tool_activity` 3/3 green (wrapper gate + builder default + settings resolution); guest suites green — xmpp 4/4 (incl. tool_activity render test), weechat 63/63, darkirc 33/33.
5. [ ] verify that work item here was addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/42

---
