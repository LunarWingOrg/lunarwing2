# Goal 34 / WI#227 — sealed interpreter step output (third attempt)

Branch: `rarity/kimi-13-20260927-0600` (re-attempt of MR !1183, which was a
re-attempt of !1152 and !1137 — all closed unmerged on review). Tracker:
[#227](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/227).

This revision answers the two findings raised on !1183 (note, 2026-09-27) and
re-raised from !1152 note #4130 / !1137 note #4053:

- **HIGH** — "capture the incident line/channel, trace the actual
  operator-response boundary, and add a runtime/bridge regression there before
  marking #227 fixed."
- **MEDIUM** — "sealed errors disappear from trace diagnostics
  (`trace.rs:250–256` detects `code_error` only for `MessageRole::User`);
  include sealed records in the analyzer and add a regression."

## The incident line and channel (captured)

The incident, as described in the WI#227 body and the goal-41 audit
(`AGENT_GOALS_2.1.3.0.md` item #46, issue #168 row): **Kageho repeatedly
reported an embedded "stale date system error" line inside operator messages
and treated it as a prompt-injection attempt.** Channel: the agent-facing
message stream (the conversation the operator/agent reads), not a log file.

The verbatim engine text behind that paraphrase is the CodeAct OS-denial
blob, produced whenever in-sandbox code calls `date.today()` /
`datetime.now()`:

```
[stdout]
OSError: OS operations are not permitted in CodeAct scripts
```

That text is generated in
`ic/crates/lunarwing_engine/src/executor/scripting.rs` (`RunProgress::OsCall`
branch) and formatted into the step blob by `format_output()` in
`ic/crates/lunarwing_engine/orchestrator/default.py`.

## Root cause, and what was already fixed before this MR

The *production* of the line stopped when the host-clock OS calls were served
(`605971dfd`, merged via !1139 — `date.today()`/`datetime.now()` now return
the host clock instead of raising `OSError`). The reviews correctly held that
this left the *carriage mechanism* unguarded: any engine-authored interpreter
or diagnostic text could still be presented as if a human had said it.

## Boundary trace — how the blob travels, and where the seal now sits

Path of one interpreter step blob through the system:

1. **Production** — `default.py::format_output(result)` renders the Monty
   result (`[stdout]`, `[stderr]`, `[name ERROR]`, `[return]` text).
2. **Working transcript** — `run_loop` records it via
   `seal_step_record(append_message(working_messages, "User", output))`. The
   `_step_record` marker is host-consumed classification metadata; it is
   stripped on decode and never persisted, logged, or forwarded to a provider.
3. **Host sync** — `executor/orchestrator.rs::sync_runtime_state` decodes
   `state.working_messages` through
   `json_to_thread_messages_with_transient` → `classify_wire_role`. Flagged
   items (and host-persisted `StepRecord` spellings) mint
   `MessageRole::SealedStepRecord`; everything unflagged keeps its wire shape
   (engine warnings, recovery nudges, bounded probes stay `User` turns). The
   decoded vec lands in `thread.internal_messages` — the inference
   transcript — not the visible one.
4. **Inference read-path** — the next orchestrator pass serializes
   `inference_messages(thread)` (internal transcript preferred) back to the
   VM; `ic/src/bridge/llm_adapter.rs::thread_msg_to_chat` maps
   `SealedStepRecord → Role::User`, so the model still reads the step data
   without the record claiming human authorship in any rendered surface.
5. **Operator-visible boundary** — the channel/operator transcript is served
   from `thread.messages` only: `ic/src/bridge/router.rs::get_engine_thread`
   (the thread-detail DTO channel UIs render). `Thread::add_message` is the
   single choke point that appends to `thread.messages`, and it reroutes
   `SealedStepRecord` into `internal_messages` instead — so no caller path,
   present or future, can place a sealed record into the served set. The
   `MessageAdded` event still fires with the `"StepRecord"` wire spelling so
   channel status hints ("Code executed", "Code error — retrying…") keep
   working (`router.rs::interpret_message_event`).
6. **Terminal operator text** — `router.rs::thread_outcome_response` renders
   `Failed { error }` as `Error: {error}`. Every `Failed` producer writes
   bounded operator prose, not interpreter text:
   `terminal_recovery_result` / `model_failure_result` (`default.py`) use
   fixed sentences ("Code execution kept failing after a final recovery
   attempt…", gateway `terminal_message`); the Rust fallback is
   `Orchestrator error: {e}` (engine error prose). The traceback itself stays
   sealed in the inference transcript (step 3).

Prior-review accuracy note (the !1152/!1183 HIGH finding): at target-branch
code, step 3 already wrote `internal_messages`, so the claim in the earlier
revisions of this document that "working_messages sync into
`thread.messages`" was wrong, and the new tests now exercise the real sync
path (`sync_runtime_state` → flagged `working_messages` → internal placement
+ visible exclusion) instead of calling `add_message` in isolation.

## What this MR changes (delta on top of !1183)

- `executor/trace.rs` — the `code_error` analyzer now matches both legacy
  `User` blobs and `SealedStepRecord` records (`NameError`, `SyntaxError`,
  `TypeError`, `NotImplementedError` in `[stdout]`/`[stderr]`/`[code `/
  `Traceback` content). Sealed interpreter errors keep producing the
  actionable warnings that `runtime/mission.rs`'s self-improvement trigger
  consumes. Regression:
  `code_error_detection_covers_sealed_step_records_and_legacy_user_blobs`.
- `executor/scripting.rs` — the Monty `context` variable now serializes
  message roles with `wire_name()` instead of `{:?}`, keeping in-sandbox role
  spellings consistent with every other wire surface.
- `executor/orchestrator.rs` (test) —
  `sync_runtime_state_seals_flagged_step_output_out_of_the_visible_transcript`:
  drives the reported boundary end-to-end (flagged `working_messages` state →
  host sync → sealed record in `internal_messages`, zero leakage into
  `thread.messages`, unflagged control text unaffected).
- `ic/src/bridge/router.rs` (test) —
  `sealed_step_records_stay_out_of_the_operator_thread_dto`: feeds a sealed
  record through `Thread::add_message` and asserts the exact set
  `get_engine_thread` serializes contains no sealed role, no sealed
  diagnostic content, and no `StepRecord` wire spelling.
- `ENGINE-V2.md` — sealed step records documented (also corrects the
  !1184-era statement that ENGINE-V2 needed no 2.1.3.0 changes).

Carried forward unchanged from !1183 (reviewed there, still correct):

- `MessageRole::SealedStepRecord` + `wire_name()` ("StepRecord"),
  `ThreadMessage::sealed_step_record`, `Thread::add_message` reroute,
  `classify_wire_role`, `seal_step_record`/`STEP_RECORD_FIELD` in
  `default.py`, `prompt_context_from_thread_messages` exclusion,
  `llm_adapter` mapping, `interpret_message_event` covering both `User` and
  `StepRecord` spellings, and the classification tests from `176609dbc`.

## Verification

- Engine lib tests (incl. the three new regressions + the carried-forward
  classification suite): see branch CI / local `cargo test -p lunarwing_engine
  --lib` output recorded at commit time.
- Bridge compile + targeted router tests:
  `cargo test --lib bridge::router` (host crate).
- `cargo clippy` on touched crates with `-D warnings`; `cargo fmt --all --
  --check`.

Compatibility: no dependency, schema, tenant-config, listener, or
init-system changes. Older persisted transcripts decode unchanged (unknown
role spellings were already `User`; `"StepRecord"` is additive wire
vocabulary). Rolling back the binary restores prior behavior.
