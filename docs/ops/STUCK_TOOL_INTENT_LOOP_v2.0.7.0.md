# Agent "stuck promising a tool call" loop — investigation (item #14)

**Date:** 2026-08-13
**Release:** v2.0.7.0
**Status:** Root cause identified; fixed in the Engine V2 orchestrator.

## Symptom

Agents accept a task ("I'll check the logs", "Let me fetch that") but never emit
the tool call. The turn ends with the promise text as the final answer, so the
user has to prompt again — sometimes several times — before the tool is actually
invoked. From the user's side this looks like the agent is stuck in a loop.

## Root cause (Engine V2, `ic/crates/lunarwing_engine/orchestrator/default.py`)

Two compounding defects in the text-response path of `run_loop`:

1. **Nudge message contradicts the CodeAct contract.** The system prompt
   (`executor/prompt.rs`, `build_codeact_system_prompt`) tells the model to call
   tools as Python functions inside ```` ```repl ```` fenced blocks. The
   tool-intent nudge and the action-obligation nudge were ported verbatim from
   the V1 loop and told the model to "Use the tool_calls mechanism" — a contract
   the CodeAct system prompt never mentions. A confused model keeps answering in
   prose, burns both intent nudges, and the loop gives up.

2. **Exhausted nudges complete with the unfulfilled promise.** When the model
   still signals tool intent after `max_tool_intent_nudges` (2), the loop fell
   through and returned the promise text as the `completed` answer. The
   action-obligation nudge (the stronger "you were asked to perform an action"
   correction) could never rescue this case because it was gated on
   `consecutive_nudges == 0` — any turn that had already consumed an intent
   nudge was permanently disarmed. The next user prompt repeated the cycle:
   promise → 2 nudges → promise returned → re-prompt.

The V1 legacy loop (`src/agent/agentic_loop.rs`) has the same
promise-exhaustion fallthrough, but its nudge wording is correct for its
provider-native `tool_calls` contract, and Engine V2 is the default runtime, so
V1 was left unchanged.

## Fix

In `default.py`'s text branch:

- Both nudges now instruct the model to respond with a ```` ```repl ```` code
  block that calls the tool as a Python function (or `FINAL(answer)`),
  consistent with the CodeAct system prompt.
- Persistent unfulfilled tool intent (`consecutive_nudges > 0` and the intent
  nudge budget spent) now escalates: it sets `obligation_enabled`, and the
  obligation nudge fires regardless of the old `consecutive_nudges == 0` gate.
- The obligation budget (`max_action_requirement_nudges`, default 2) still
  bounds the escalation, so a model that never complies still terminates with
  its text — the loop cannot nudge forever.

Net effect: a turn gets up to 2 intent nudges + 2 obligation nudges with
CodeAct-correct instructions before falling back to the previous behavior,
which resolves the multi-prompt loop for models that merely needed a consistent
contract.

Note: tenants running self-improved orchestrator versions (v1+ memory docs via
`load_orchestrator`) keep their patched script until it fails over or is
rebased; the fix ships in the compiled-in v0 baseline, which is also the
automatic fallback.

## Regression coverage (`crates/lunarwing_engine/src/executor/loop_engine.rs`)

- `tool_intent_nudge_injected` — extended: the nudge must mention ```` ```repl ````.
- `persistent_tool_intent_escalates_to_action_obligation` — three promise texts
  trigger 2 intent nudges + 1 obligation nudge, then a code response completes.
- `unfulfilled_intent_completes_after_all_nudges_exhausted` — with both budgets
  spent (2+2), the loop still terminates with the final text response.
