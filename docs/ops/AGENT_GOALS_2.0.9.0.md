# PRE-RELEASE CHECKLIST for LunarWing v2.0.9.0

## Codename: Inazuma (稲妻)

**Open TODOs (v2.0.9.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)

### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] investigate work item and see if issue still persists and fix if necessary: https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/55 — DONE 2026-08-24 (rarity/kimi-1-20260824-0001): issue #55 ("write to memory.md does not work without certain env vars set — unsure of exact env var but one of tool ones") does NOT persist as described; no env var ("tool" or otherwise) gates memory.md writes in current code. Audited every layer that could block `memory_write`: (1) registration — memory tools register unconditionally whenever the DB exists (`ic/src/app.rs:375-433`; DB connect failure is a hard startup error, so a running daemon always has them); (2) permissions — seeded default for memory_write is `AlwaysAllow` (`ic/src/tools/permissions.rs:45`); (3) approval — MemoryWriteTool uses the trait default `ApprovalRequirement::Never`, so the chat loop never prompts for it (`ic/src/agent/dispatcher.rs:689`); (4) autonomous/job/routine contexts — memory_write is not on `AUTONOMOUS_TOOL_DENYLIST` and lightweight-routine filtering keeps Never-approval tools (`ic/src/tools/autonomy.rs`, `ic/src/tools/registry.rs:461`); (5) engine v2 — all registered tools are exposed via the "tools" capability and executed through the same `execute_tool_with_safety` pipeline (`ic/src/bridge/router.rs:1145-1162`, `effect_adapter.rs:804`); (6) web gateway `/api/memory/write` — env-independent (`ic/src/channels/web/handlers/memory.rs:139`); (7) workspace write path — no env vars, only injection scanning (`ic/src/workspace/mod.rs:917`). The ONE mechanism that silently removes memory_write from the LLM's tool list is skill trust attenuation: when a registry-installed (Installed-trust) skill is active, the read-only ceiling strips memory_write while memory_read/search/tree remain (`ic/src/skills/attenuation.rs`) — an intentional security control, and the only plausible match for the reported symptom (memory readable but not writable). It was previously invisible at default log level (per-iteration `debug!` only), which explains why the reporter could not tell what happened; added a once-per-turn `info!` log naming the removed tools (`ic/src/agent/dispatcher.rs`) so the next occurrence is diagnosable without debug logs. Remediation for the original report: deactivate the installed skill (or `SKILLS_ENABLED=false`), and check for a stale per-user `tool_permissions.memory_write` override in DB settings. Verified empirically with zero special env vars: `cargo test --lib` memory (59 passed), attenuation (5 passed), tools::permissions (14 passed) — all green, zero warnings
2. [x] verified addressed: XMPP agent image attachments are base64-encoded by the WASM channel, decoded by the bridge, queued for XEP-0363 upload, and `/v1/messages/send` has a scoped 70 MiB body limit with pre-body authorization; bridge and native attachment regression tests pass: https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/42
3. [ ] First, ensure that lunarwing2study mcp that is built into the agent is updated with the LATEST version of the knoweldge store/notes - can be found at: https://git.lunarwing.org/cmc/lw2studymcp.git - then create a mechanism that NEWLY created tenants can have their knowledge stores filled with the contents of the repo's database (dynamically). verify correctness by creating a new tenant. this process should also not populate/repopulate an existing tenant's knowledge store or overwrite it. verify this by creating a tenant on an older branch, then upgrading it using upgrade-tenant with a target of the working branch
4. [x] (S) reserved for ci/cd failures
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/73
6. [x] (S) reserved
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/65
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/72
9. [x] (S) reserved
10. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
11. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
12. [ ] update anything outdated in docs/proposals - verify against code rather than docs
13. [x] (S) relnotes
14. [x] (S) update release notes draft in root of repo with anything might have missed that may be relevant and update README.md in repo root

---
