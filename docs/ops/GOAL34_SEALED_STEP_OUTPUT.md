# Goal 34 / WI#227 — sealed interpreter step output (re-attempt)

Branch: `aj/kimi-34-20260925-0601` (MR !1152, closed unmerged); landed on
2.1.3.0 via item #13 re-attempt branch `rarity/kimi-13-20260926-2102`. Tracker:
[#227](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/227).
Review thread: [!1137](https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1137).

## What this attempt addresses

WI#227 reports an engine-authored internal error line ("stale date system
error") appearing inside user-visible message content, which Kageho read as a
prompt-injection attempt. Two root causes are already fixed and merged
(!1139, `605971dfd`): the CodeAct OS-call denial that produced that exact
error line, and the REPL argument/fence integrity guard (WI#161, `6635f1422`).
The !1137 review's HIGH finding — "the WI is not yet demonstrated fixed; the
tests exercise CodeAct return values, not the reported transcript/message
boundary" — remained open because the *mechanism* that carries any
engine-authored interpreter or diagnostic text into user-visible transcripts
was still unguarded:

1. The Python orchestrator records every CodeAct step blob (`[stdout]`,
   `[name ERROR]`, `[return]`) as a role-`User` message in its working
   transcript. That transcript syncs into `thread.messages` through the wire
   decoder's `_ => ThreadMessage::user(content)` fallback.
2. `ThreadOutcome::Failed { error }` renders verbatim into channel text
   (`thread_outcome_response`), and interpreter `OSError`/diagnostic text
   reaches that field through `terminal_recovery_result` /
   `model_failure_result` error strings built from step output.

## Changes

Commit `8c31b110e` seals machine-produced step output at the
model-output/executable-transcript boundary, host-side:

- New `MessageRole::SealedStepRecord` with `wire_name()` = `"StepRecord"`, and
  `ThreadMessage::sealed_step_record` (host-minted; exhaustive matching, so a
  future role cannot silently widen the set).
- `Thread::add_message` reroutes that role into `internal_messages`: the
  user-visible transcript can no longer contain a sealed record, whatever
  caller path produced it.
- `classify_wire_role` (engine `executor/orchestrator.rs`) owns role
  classification when orchestrator wire state decodes. Only two shapes the
  Python VM cannot forge classify as sealed: the host-persisted `StepRecord`
  spelling, and items the default orchestrator flagged `_step_record`.
  Everything unflagged keeps its wire shape — engine warnings, bounded probes,
  recovery nudges, and retrieved context stay effective `User` turns, so no
  behavioral change lands on legitimate machine-to-model controls.
- `orchestrator/default.py` marks freshly produced `format_output` blobs with
  `_step_record` via `seal_step_record`; the flag is consumed by the host
  decoder and never persisted, logged, or forwarded to the provider.
- Bridge role spellings (`bootstrap_messages`, trace records,
  `get_engine_thread` DTO, thread events) move from Rust `Debug` formatting to
  `wire_name()`; `prompt_context_from_thread_messages` excludes sealed records
  from delegated-worker context; the inference adapter maps
  `SealedStepRecord → Role::User` (inference still reads the data; it never
  claims human authorship, and the existing WI#161/G8 neutralization stack
  still applies downstream).
- Diagnostic `Failed` error strings are bounded operator prose, and the
  interpreter traceback text stays sealed inside the inference transcript.

Compatibility: no dependency, schema, tenant-config, listener, or init-system
changes. Older persisted transcripts decode unchanged (unknown spellings were
already `User`; `"StepRecord"` is new additive wire vocabulary). Rolling back
the binary restores prior behavior.

## Verification

- Engine lib suite: **628 passed / 0 failed**, including the new regression
  `sealed_step_records_classify_host_side_and_unflagged_writes_stay_user`
  (classification of both sealed shapes, visible-transcript exclusion +
  internal placement, unflagged writes staying `User`, flag stripping on
  decode) plus the full WI#161 stream-integrity, orchestrator, scripting, and
  thread suites.
- `cargo clippy -p lunarwing_engine --lib --tests -- -D warnings`: zero
  warnings. Host lib `cargo check` + `clippy -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.
- `python_syntax.py`: 111 tracked files valid.
- `check_no_panics.py` vs merge-base `d170052a5`: clean.
- Runtime transcript/bridge boundary tests (`loop_engine`,
  `engine_v2_tensorzero_streaming` module compile-checked via lib/tests)
  unaffected: no test asserted that step blobs appear in `thread.messages`.

## Remaining (operator, unchanged from !1137 scope)

Kageho live-tenant confirmation that no embedded internal-error line appears
in operator messages on the patched build. The deterministic replay steps in
`GOAL34_REPL_ARGUMENT_INTEGRITY.md` cover the payload classes; the seal now
also covers the transcript-boundary class the review flagged. Tracker closure
of WI#227 remains with the filer.
