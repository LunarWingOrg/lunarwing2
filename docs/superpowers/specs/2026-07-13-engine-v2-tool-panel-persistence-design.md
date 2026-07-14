# Engine V2 Tool Panel Persistence Design

## Status

Approved direction, awaiting review of this written specification.

This design corrects and manually ports the useful parts of commit `6aed96e`
from `fix/ui/toolpanel-gui-fix-2` onto the current Phase 5 integration branch.
The source commit is a reference, not a cherry-pick target, because its router
logic predates channel-neutral status delivery and approval-resume fixes.

## Problem

Engine V2 tool cards are incomplete in the gateway UI:

1. A successful live tool card expands to an empty body because Engine V2
   emits `ToolStarted` and `ToolCompleted`, but no `ToolResult` containing the
   tool output.
2. Tool cards disappear after a refresh or thread switch because Engine V2
   persists the user and assistant rows, but no intervening
   `role="tool_calls"` history row.

The frontend already handles both contracts. `setToolCardOutput` consumes a
live `tool_result` event, and `createToolCallsSummaryElement` renders tool call
summaries returned by the history endpoint. The missing data originates in the
Engine V2 event and bridge paths.

## Goals

- Populate successful live tool cards with a bounded output preview.
- Persist tool summaries so they survive refreshes and thread switches.
- Preserve tools executed before and after an approval or authentication
  pause in the same completed turn.
- Keep Phase 5 progress delivery channel-neutral and emit each gateway status
  exactly once.
- Keep persisted output bounded and backward-compatible with existing engine
  event data.
- Preserve terminal response, interrupt, approval, authentication, and channel
  rollout behavior.

## Non-Goals

- Showing complete, unbounded tool output in the browser or database.
- Changing the gateway HTML, JavaScript, or CSS.
- Redesigning the general Engine V2 event store or v1 history schema.
- Enabling Engine V2 for DarkIRC or WeeChat.
- Changing which tools require approval or authentication.
- Reworking existing tool-output sanitization policy.

## Design Decision

The fix has three connected parts:

1. Carry a bounded successful result preview on `EventKind::ActionExecuted`.
2. Translate that preview to `StatusUpdate::ToolResult` through the current
   Phase 5 channel status path.
3. Build the reloadable `tool_calls` history row from the saved terminal thread
   rather than from an ephemeral broadcast receiver.

```text
post-safety ActionResult.output
    -> ActionExecuted.result_preview (at most 1000 bytes)
       -> live: ChannelManager::send_status(ToolResult)
          -> gateway status mapper -> one SSE tool_result event
       -> terminal: saved Thread.events
          -> bounded tool-call summaries (at most 500 bytes each)
          -> v1 history: user -> tool_calls -> assistant
```

The frontend remains a consumer of its existing event and history contracts.

## Engine Event Contract

`EventKind::ActionExecuted` gains:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
result_preview: Option<String>
```

The field is optional so stored events created before this change continue to
deserialize. A shared helper converts an `ActionResult.output` value to a
preview:

- `null` and an empty string produce `None`;
- strings remain strings;
- other values use compact JSON serialization;
- values longer than 1000 bytes are truncated at a UTF-8 boundary and receive
  the existing ASCII `...` suffix.

Every real successful execution site in the structured, scripting, and
orchestrator paths populates the preview. Synthetic event constructors that do
not have an output use `None`.

## Live Delivery

`forward_event_to_channel` maps a successful `ActionExecuted` event to:

1. `StatusUpdate::ToolStarted`;
2. `StatusUpdate::ToolResult` when the preview is non-empty;
3. `StatusUpdate::ToolCompleted`.

The same formatted display name is used for all three updates so the frontend
matches the result to the correct live card.

The current Phase 5 architecture already maps gateway status updates to SSE.
`thread_event_to_app_events` therefore remains structural-only for this event;
it must not regain the old direct tool-event mapping from commit `6aed96e`,
which would duplicate gateway events.

Best-effort status delivery remains non-fatal. Other enabled channels receive
the same typed status and retain their existing transport-specific handling.

## Approval Resume

An approved pending action may execute directly through the bridge before the
engine thread resumes. That path must record the same `ActionExecuted` event as
ordinary engine execution so the tool is visible live and in history.

A narrow ThreadManager operation will append the externally resolved action
event to the saved thread and broadcast it through the normal engine event
sender. The bridge subscribes before executing the pending action, records the
successful result using the original action name and call ID, and then resumes
the thread with that result. This preserves one event and one UI card without
inventing a second gateway-only delivery path.

Denied or cancelled actions do not become successful tool records. Existing
failed action events continue to provide error summaries.

## Durable History Reconstruction

After `join_thread` completes and the manager has saved the final thread,
`await_thread_outcome` loads that thread and derives tool summaries from its
durable event list. It does not accumulate summaries from `event_rx`.

This choice matters because:

- broadcast receivers can lag and skip events;
- each approval resume creates a fresh receiver;
- the saved thread spans the entire turn, including events before and after a
  gate pause.

For each completed or failed action, the bridge creates a summary containing:

- `name`;
- `call_id` when available;
- `result_preview` truncated to 500 bytes for successful output;
- `error` truncated to 200 bytes for failures.

Full parameters are not copied into this UI history row. This avoids retaining
potentially sensitive inputs, and the existing v1 hydration fallback treats
missing parameters as an empty object. The bounded preview is sufficient for
both UI rendering and the existing fallback result reconstruction.

When a terminal outcome has non-empty text, the bridge writes the summary row
immediately before the assistant row. Gate pauses and stopped turns write
neither row. A later successful resume reconstructs from the complete saved
thread and writes all tool summaries once with the terminal assistant response.

Failure to load or serialize tool summaries is logged and does not suppress a
valid terminal response. Database writes remain best-effort, matching the
existing v1 compatibility history behavior.

## Compatibility And Retention

- Older stored `ActionExecuted` events deserialize with `result_preview=None`.
- No database migration is required; `tool_calls` remains a JSON message row.
- Existing v1 turns and Engine V2 turns without tools render unchanged.
- Engine V2 remains enabled only for the currently configured channel set.
- Engine event previews are capped at 1000 bytes; reloadable UI previews are
  capped at 500 bytes; the frontend retains its existing live display cap.
- The preview is derived from the post-safety `ActionResult` supplied to the
  engine. This change does not bypass the existing tool safety wrapper.

## Testing Strategy

Tests are written and observed failing before production changes.

### Engine tests

- String and compact JSON output produce previews.
- Null and empty output produce no preview.
- ASCII and multibyte output truncate safely.
- An old serialized `ActionExecuted` value without `result_preview`
  deserializes successfully.
- Structured, scripting, and orchestrator execution events carry the preview.

### Bridge tests

- A successful action with output emits
  `ToolStarted -> ToolResult -> ToolCompleted` through `ChannelManager`.
- An empty preview omits only `ToolResult`.
- The direct gateway event translator does not duplicate those statuses.
- A saved thread containing actions on both sides of an approval event
  reconstructs all tool summaries in call order.
- Successful and failed summaries round-trip through
  `build_turns_from_db_messages`.
- A directly executed approved action is recorded with its original call ID
  and appears once.
- Gate-paused and stopped outcomes do not prematurely persist a tool row.

### Verification

- Run the narrow engine and bridge regression tests from `ic/` with six-thread
  limits.
- Run formatting, targeted compile checks, and relevant Clippy checks without
  a full debug build.
- Rebuild Brightdawn only as a release deployment after automated checks pass.
- Live gateway gate: execute a tool, expand its live card, refresh, switch away
  and back, and confirm the same bounded preview remains visible.
- Recheck one supervised approval turn to confirm the approved tool appears
  once and the approval loop does not regress.

## Integration Strategy

Do not cherry-pick `6aed96e`. Manually port only its event-preview intent and
adapt it to the current router and approval flow. The obsolete direct SSE
translation and receiver-local tool accumulator are explicitly excluded.

The implementation stays scoped to the engine event/execution paths, the
ThreadManager event-recording boundary, the Engine V2 router, focused tests,
and this specification. No unrelated UI or Phase 5 rollout changes are part of
the fix.
