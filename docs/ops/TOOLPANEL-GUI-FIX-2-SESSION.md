# Tool Panel GUI Fix — Session Summary

- **Branch:** `fix/ui/toolpanel-gui-fix-2`
- **Date:** 2026-07-13
- **Scope:** Backend only (engine crate + bridge). No frontend changes, no commit made.

## Problem

In the gateway web GUI (`ic/src/channels/web/static/`), the per-turn tool display was broken in two ways:

1. **Live tool cards expanded to an empty box.** During a turn the card appeared, but expanding it showed no output (only failed tools showed anything).
2. **The tool section vanished entirely** when the chat was re-rendered — switching threads or refreshing. Switching tabs kept it (that path is CSS-only, so it was never affected).

## Root Cause

Both symptoms came from the newly-wired engine path (`feat/wire-engine-p345`) dropping tool-call fidelity that the old dispatcher had. The **frontend was already correct** — it fills a live card body from a `tool_result` SSE event (`app.js` `setToolCardOutput`) and re-renders reloaded tools from history `tool_calls` (`createToolCallsSummaryElement`). The backend engine path simply never produced either.

- `EventKind::ActionExecuted` (`ic/crates/lunarwing_engine/src/types/event.rs`) carried only `action_name`, `call_id`, `duration_ms`, `params_summary` — **no tool output** — even though the output (`ActionResult.output`) was in scope at every real execution site.
- The bridge translators `thread_event_to_app_events` and `forward_event_to_channel` (`ic/src/bridge/router.rs`) therefore emitted only `ToolStarted` + `ToolCompleted`, never `ToolResult`. On success, `completeToolCard` never writes body text → empty box.
- `await_thread_outcome` (`ic/src/bridge/router.rs`) persisted only the `user` and `assistant` messages — it never wrote a `role="tool_calls"` record. On reload, `build_turns_from_db_messages` (`ic/src/channels/web/util.rs`) saw `user → assistant` with nothing between, so `turn.tool_calls` was empty and no tool section rendered.

The old dispatcher did both: `agent/dispatcher.rs` emitted `ToolResult`; `agent/thread_ops.rs` `persist_tool_calls` wrote the `tool_calls` record. The engine port lost them.

Confirmed the engine path does **not** populate the v1 in-memory `Session.turns`, so engine turns fall through to the DB history path — persisting a DB `tool_calls` record is sufficient for reload.

## Changes (6 files)

**Engine — carry the tool output through the event:**
- `crates/lunarwing_engine/src/types/event.rs` — added `result_preview: Option<String>` to `EventKind::ActionExecuted` (`#[serde(default, skip_serializing_if = "Option::is_none")]` so previously-persisted events still deserialize); added `preview_from_output(&Value, max)` helper (string outputs verbatim, other JSON compact-serialized, truncated at a UTF-8 char boundary, `None` when empty).
- `crates/lunarwing_engine/src/executor/scripting.rs` — populate preview from `result.output` in `resolve_tool_future`.
- `crates/lunarwing_engine/src/executor/structured.rs` — populate from `action_result.output` in `classify_exec_result`.
- `crates/lunarwing_engine/src/executor/orchestrator.rs` — populate from `r.output` at both execution sites; `None` at the synthetic replay constructor.
- `crates/lunarwing_engine/src/executor/trace.rs` — `None` in the test event builder.

**Bridge — emit it live and persist it (`ic/src/bridge/router.rs`):**
- `thread_event_to_app_events` + `forward_event_to_channel` now emit `ToolStarted → ToolResult → ToolCompleted` (ToolResult only when a non-empty preview exists), reusing the same display name so the frontend matches the card. → fixes the empty live card.
- Added `collect_tool_call(&ThreadEvent, &mut Vec<Value>)` helper producing the JSON shape `{name, result_preview?, error?}` expected by `build_turns_from_db_messages`.
- `await_thread_outcome` accumulates summaries from `ActionExecuted`/`ActionFailed` as events stream (both the `recv` and drain arms), then writes a `role="tool_calls"` record to the same v1 conversation immediately before the assistant message (order `user → tool_calls → assistant`). → fixes vanishing on reload/thread-switch.

Only `ActionExecuted` gained a field; `ActionFailed` already carried `error`, which the frontend already renders.

## Tests Added (regression, per repo policy)

- Engine (`event.rs`): `preview_from_output` handles strings/JSON/empty and respects UTF-8 boundaries.
- Bridge (`router.rs`): `ActionExecuted` with a preview emits `[ToolStarted, ToolResult, ToolCompleted]`; without (or empty) emits `[ToolStarted, ToolCompleted]`; `collect_tool_call` output round-trips through `build_turns_from_db_messages`.

## Verification

- `cargo check` — clean.
- `cargo clippy --all --tests` — zero warnings; `--all-features` also zero warnings (run via `ext_proxy` to work around an openssl download timeout).
- `cargo test -p lunarwing_engine --lib` — 316 passed (includes existing `ActionExecuted` serde/replay/trace tests exercising the new field).
- New regression tests — 5 passed (2 engine, 3 bridge).
- Existing history reconstruction tests (`build_turns*` in `util.rs` + `chat.rs`) — 14 passed.

## Known Limitations / Follow-ups

- **No live browser verification** — a full gateway (Postgres + LLM keys) was not runnable in this environment; verification was compile + unit tests only. Recommended manual pass: run a tool → expand the live card and confirm output is visible → refresh / switch threads and confirm the "N tools used" section reappears and expands.
- `scripts/check-boundaries.sh` reports 4 pre-existing hard violations in `src/llm/` (`transcription/mod.rs`, `oauth_helpers.rs`) — unrelated to this change; left untouched.
- No commit was made in this session.
