# GitLab Work Items Review — v2.0.7.0

- **Generated**: 2026-08-12 (release checklist item #11 from `AGENT_GOALS_2.0.7.0.md`)
- **Source**: GitLab work items at `git.lunarwing.org`, project `lunarwing/lunarwing2` (ID 35), read via the unauthenticated REST API (`/api/v4/projects/35/issues`). Issue **notes/comments require auth** and were not readable; verdicts are based on issue bodies plus repository verification.
- **Method**: every open item was checked against current source on branch `rarity/kimi-11-20260812-1200`, prior audits (`SUNBURST_BUG_LIST.md`, `ISSUE_STATUS_REPORT.md`, `DARKIRC-KEYEXCHANGE-STATUS.md`), and recent git history. Existing closed items were not re-audited.
- **Snapshot totals**: 47 issues fetched — 12 open, 35 closed.

## Verdict legend

- 🔒 **Recommend close** — confirmed fixed or otherwise resolved; closing evidence cited.
- 🟠 **Close candidate (needs owner confirmation)** — likely addressed, but the issue lacks enough description to prove it.
- 🟡 **Keep open** — defect/feature verified as still present or intentionally pending.

## Issues recommended for closure

| # | Title | Verdict | Evidence |
|---|-------|---------|----------|
| #26 | [BUG-005] Multica daemon connection refused on 127.0.0.1:6680 | 🔒 **Close as duplicate of closed #25 ([BUG-006])** | The 2026-08-02 repository audit (`SUNBURST_BUG_LIST.md`) established BUG-005 is **misclassified**: no Multica source uses port 6680 or `/poll`; both belong to the DarkIRC adapter (`ic/channels-src/darkirc/src/lib.rs:10`, `:307-311`, `darkirc_adapter.py:50`). The underlying defect is BUG-006 ("DarkIRC adapter not deployed as managed service"), which is **fixed in source** — `lunarwing-mt-admin.sh` renders/enables/starts the per-tenant DarkIRC adapter on both systemd-user and OpenRC (`ic/scripts/lunarwing-mt-admin.sh:6409-6428`, `:7192-7253`) — and its own GitLab issue #25 is already closed. Deployed tenants pick the fix up through `render-units` / normal upgrade deployment. Nothing Multica-side remains to track here. |
| #48 | http tool doesn't work in WeeChat (likely DarkIRC too) | 🔒 **Recommend close — reproduced failure fixed** | Tool execution and response routing work from WeeChat; the reproduced failure was a cross-extension credential collision. A WASM extension with `host_patterns: ["*"]` published that mapping into `SharedCredentialRegistry`, so unrelated built-in `http` requests entered Engine V2's missing-credential flow and could receive the extension's secret. `SharedCredentialRegistry::add_mappings` now keeps bare `*` mappings local to the owning WASM sandbox while retaining exact and `*.domain` mappings for built-in HTTP injection. Regressions cover registry matching, HTTP approval classification, and Engine V2 auth preflight. Issue #47 remains separate: WASM channels still suppress tool-activity status events. |

## Close candidates pending owner confirmation

| # | Title | Verdict | Evidence |
|---|-------|---------|----------|
| #2 | LunarVision inbound hook | 🟠 **Likely implemented; issue body is empty, so intent cannot be confirmed** | Current source contains an automatic LunarVision enrichment path for inbound images: `ic/src/vision_enrichment.rs` ("Automatic LunarVision enrichment for inbound XMPP images"), wired into the agent loop (`ic/src/agent/agent_loop.rs:249`), with loopback-only URL validation, 10 MiB image cap, and bounded concurrency. If #2 meant "analyze inbound channel images with LunarVision automatically," that capability now exists and the issue can be closed. Because the issue has no description, owner confirmation is required before closing. |

## Issues that stay open (verified still present or intentionally pending)

| # | Title | Verdict | Evidence |
|---|-------|---------|----------|
| #47 | [Feature] tool_activity flag to route StatusUpdate tool events to WASM channels | 🟡 **Keep open** — feature not implemented | Verified: `debug_mode` exists only in native channels (`ic/src/channels/signal.rs`, `ic/src/channels/repl.rs`); no `tool_activity` config flag exists anywhere in `ic/src`, and WASM channels still do not consume `StatusUpdate::{ToolStarted, ToolCompleted, ToolResult}`. |
| #45 | darkirc hsm key exchange | 🟡 **Keep open** — Stage A implemented, acceptance pending | Per `DARKIRC-KEYEXCHANGE-STATUS.md` (baseline `fb548a6`): Stage A source, audit fixes, and local validation are complete; live OpenRC/systemd-user H3 acceptance runs, final release CI, and Stages B–D remain pending. The issue itself records "progress: stage a". |
| #42 | XMPP Bridge Issue — agent cannot attach images and send correctly | 🟡 **Keep open** — confirmed bug, no fix landed | Labeled `bug, confirmed, high`. No xmpp-bridge attachment fix commits since the 2026-08-07 report; subsequent `ic/bridges/xmpp-bridge/` changes are 2.0.7 version bumps and lockfile refreshes only. |
| #40 | image_generate tool: double /v1/v1/ path in API route (404) | 🟡 **Keep open** — defect verified in current source | `ic/src/tools/builtin/image_gen.rs:126-129` unconditionally builds `"{base}/v1/images/generations"` from the configured provider base URL. When the provider base URL already ends in `/v1` (e.g. a TensorZero-style `…/openai/v1`), the request path becomes `/openai/v1/v1/images/generations`. `/v1` suffix normalization exists only in `ic/src/llm/lunarwing_cloud_chat.rs:162`, `:829` — not in the image tool — and `image_gen.rs` is unchanged since the first v2 commit. (Same construction exists in `image_edit.rs`.) |
| #41 | NanoGPT MCP: list_image_models returns count but no model names | 🟡 **Keep open** — external component, not verifiable here | No `list_image_models` / `image-models` code exists anywhere in this repository; the NanoGPT MCP server is an external component. No evidence of a fix reachable from this tree. |
| #4 | Reflex Compiler Testing Suite | 🟡 **Keep open** — validation checklist largely uncompleted | Matches historical GitHub #5 in `ISSUE_STATUS_REPORT.md`: the reflex compiler is implemented with basic tests, but the full 8-phase validation checklist was never completed. Item #3 of this release fixed specific reflex/orchestrator defects; the broad validation suite this issue tracks is still undone. |
| #3 | [OpenRC] Proposed changes — To Defer | 🟡 **Keep open** — deferred by intent | Title marks it deferred; no description to verify against. |
| #1 | old issues from v1 never addressed | 🟡 **Keep open** — tracking bucket | Meta issue with no description. v1-era issues were triaged in `ISSUE_STATUS_REPORT.md` (2026-07-09 snapshot); this bucket tracks the remainder. |
| #5 | ALL | 🟡 **Keep open** — meta/tracking | No description; appears to be an aggregate tracking issue created at repo import (2026-07-23). |

## API access notes for future reviewers

- Unauthenticated read works for the project and issue list/detail endpoints:
  `GET /api/v4/projects/lunarwing%2Flunarwing2`, `GET /api/v4/projects/35/issues?state=all&per_page=100`.
- The newer `/api/v4/projects/35/work_items` endpoint returns 404 on this instance; use `/issues`.
- Issue **notes** return `401 Unauthorized` without a token, so comment-level status (e.g. the 2026-08-12 activity on #48) cannot be read anonymously.
- Closing issues requires a token; this review makes recommendations only.

## Summary

- **Recommend close now: 2** — #26 (duplicate of fixed/closed #25) and #48 (cross-extension credential collision fixed).
- **Close after owner confirmation: 1** — #2 (inbound LunarVision enrichment exists in `vision_enrichment.rs`).
- **Verified still open with evidence: 9** — #47, #45, #42, #40, #41, #4, #3, #1, #5.
