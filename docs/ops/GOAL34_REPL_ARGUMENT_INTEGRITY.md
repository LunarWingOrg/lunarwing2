# Goal 34 / WI#161 — REPL argument integrity

Branch: `a/goal34/v2.1.2.0`. Tracker:
[#161](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/161).

## Findings

The incident is intermittent and the tracker has no raw provider SSE capture.
It is therefore not evidence of a confirmed reasoning/argument race. Source
review found three concrete problems:

1. `StreamIntegrityProvider` inspected assistant text but passed tool-argument
   deltas through without inspection. A valid JSON object containing corrupt
   Python and a void/null argument payload were both outside its guard.
2. The CodeAct extractor searched for the next triple-backtick substring. A
   nested opening fence could truncate a malformed draft to a **valid executable
   prefix**; backticks inside a Python string could also terminate extraction.
3. `RigStreamState` marked both name and argument deltas as “seen.” A name-only
   delta followed by a full tool-call event lost all the final arguments.
   Conversely, argument-only deltas followed by a full call lost its name.

Reasoning deltas already have a separate typed path and are discarded by the
Rig adapter rather than appended to tool arguments. The legacy
`reasoning.rs::clean_response` text cleaner is not used by this native Engine
V2 streaming path. Stripping more tags there would not fix REPL arguments.
Upstream Context7 documentation describes a newer Rig streaming API; the
implementation and tests use this repository's vendored Rig 0.40.0 API.

## Changes

- Preserve final arguments after name-only/empty deltas; preserve the final
  name after argument-only deltas. Already-streamed argument bytes are not
  duplicated or replaced by a reconstructed upstream payload.
- Validate REPL JSON shape and inspect executable Python with a lexical shield
  for quoted strings/comments. Both standalone closing tags and complete
  reasoning/argument markup in executable positions reject the draft.
- Use complete, line-delimited CodeAct fences, preserving the existing
  `repl` → `python` → `py` → bare-fence selection priority. Nested openers and
  incomplete/empty blocks do not become executable prefixes.
- Return the distinct `repl_argument_corruption` failure and a fixed repair
  instruction. One repair is allowed; repeating the corruption fails clearly.
  The rejected response's tools never run, even in mixed tool batches.
- Log bounded metadata (`error_class`, signature, provider), not generated
  arguments. Export `repl_argument_corruption_total` through the existing
  per-provider JSON snapshot and tenant-aware health/Prometheus pipeline.
- Retain Monty's syntax-error correction for ordinary invalid Python (including
  malformed `__main__` guards or untagged prose); do not guess repairs that might
  change a program's effects.

There are no dependency, database-schema, tenant-config, listener, or init-system
changes. Rolling back the binary restores the previous behavior; older metric
readers ignore the additive JSON field, and current readers default missing
fields to zero.

## Verification

- Engine unit/runtime suite: **592 passed**, including rejection before effects,
  bounded correction, tag/fence literals, nested/unclosed fences, malformed/void
  arguments, and language-selection compatibility.
- Healthpoint/Prometheus harness: **18 passed**, including tenant aggregation
  and the provider/tenant-labeled REPL corruption series.
- Host LLM/provider suite: **598 passed**, including name-only and argument-only
  delta assembly, interleaved reasoning isolation, byte-fragmented corruption,
  blocking/fallback parity, and exact counter increments.
- Bridge LLM adapter suite: **13 passed**.
- ShellCheck on the changed healthpoint and harness: passed.
- Real HTTP/SSE integration: **6 passed** (`engine_v2_tensorzero_streaming` with
  `libsql,integration`). The new regression replays null, malformed JSON,
  think/argument-tag pollution, and nested-fence arguments through the native
  OpenAI-compatible SSE adapter, bridge, engine, and gateway test channel. Every
  case repairs once, invokes the real test tool exactly once, and delivers one
  terminal response; rejected drafts/call IDs never enter the repair transcript.
- Clippy `-D warnings`: engine lib/tests and daemon lib passed. Rustfmt and
  `git diff --check` passed. Cargo used vendored/offline dependencies, at most
  12 CPUs/jobs, and debug-info-disabled test/check profiles in tmux.
- Required fresh live tenant: **pending privileged host access**. The coding
  session runs as `paseo` with `NoNewPrivileges`; `sudo -n` fails before mt-admin
  can create the account. No new tenant has been provisioned by this session.

## Live acceptance

Resolved 2026-09-18 (`aj/kimi-34-20260918-0901`, running as `applejack` with working sudo).

A fresh tenant `goal34repl` was provisioned, built, and started through
`ic/scripts/lunarwing-mt-admin.sh`. Its `ic/target/release/lunarwing` binary
contains the `repl_argument_corruption` code (symbols present), and the gateway
(port 10030) answers `/api/health` with `200`. The tenant's `LLM_BASE_URL` points
at a loopback OpenAI-compatible SSE fixture (`/tmp/goal34/fixture.py --port 18999`),
which deterministically replays the corrupted payloads and asserts the fix
behavior. The acceptance driver is `/tmp/goal34/acceptance.sh`; results land in
`/tmp/goal34/acceptance.json`.

Result: **13 pass / 5 fail**. The core of WI#161's requested action works live:
the corruption is detected and counted at the stream layer, and the live metric
`repl_argument_corruption_total` increments (`>= 3`, observed 3). The 5 failures
are a harness-design limitation, not a regression in the merged fix:

- The fixture emits a **native OpenAI tool-call named `repl`** (a `tool_calls`
  delta with `function.name == "repl"` and a corrupted `code` argument). `repl`
  is a synthetic action name, not a registered capability, so it has no lease on
  a fresh tenant. Any surviving (even clean) native `repl` call therefore fails
  at the executor capability gate with `No lease for action 'repl'`, seen in the
  gateway events `tool_started name=repl` → `tool_completed success=false
  error="No lease for action 'repl'"`.
- Real CodeAct REPL is the ` ```repl ` **text fence**, parsed by
  `LlmResponse::from_text` → `LlmResponse::Code` (`types/step.rs:111`), which is
  a *different* representation from the native tool-call path. The five failed
  assertions (repair instruction present, rejected draft absent, no tool result
  from rejected call, `tool_result` event delivered, structured warnings in
  journal) are all downstream of that gate short-circuit, not of the corruption
  guard itself.
- The in-process real-SSE integration test
  `corrupted_repl_arguments_recover_through_real_sse_without_executing`
  (`ic/tests/engine_v2_tensorzero_streaming.rs`) already exercises the tool-call
  repair path end-to-end with a *registered* test tool, where the repair cleanly
  re-invokes and the guards are all asserted to pass.

Correcting the live fixture (replay the text-fence corruption path rather than a
native `repl` tool call, or register a real capability named `repl`) would clear
the remaining failures, but that is a test-harness change and does not affect the
shipped fix. The goal stays unchecked pending that correction or a policy
decision that `repl` should be leased.

Session-local host helper: `/tmp/opencode/goal34-live-host.sh`; deterministic
fixture/client: `/tmp/opencode/goal34-live.py`. Their shell/Python syntax checks
pass, but the live helper has **not** been executed successfully. From a
privileged host terminal, run it in tmux:

```bash
sudo tmux new-session -d -s goal34-live \
  'bash /tmp/opencode/goal34-live-host.sh'
```

It refuses to overwrite an existing `goal34repl` account, provisions/builds/starts
through mt-admin, copies the local fix into that fresh tenant's
checkout, and writes a sanitized status to
`/tmp/opencode/goal34-live-host-status.json`. Successful acceptance additionally
writes `/tmp/opencode/goal34-live-evidence.json`; provisioning/build logs remain
root-private. The live run must succeed before checking off Goal 34.

The live acceptance must use a newly provisioned tenant (`goal34repl`) running
the patched release binary, managed through `ic/scripts/lunarwing-mt-admin.sh`.
Replay the rare incident deterministically with a loopback OpenAI-compatible
SSE fixture, through the tenant's authenticated gateway:

1. Send fragmented null/malformed/think-tag/fence-contaminated `repl` arguments
   and a nested text-fence draft with an effectful valid prefix.
2. Assert the next provider request contains the fixed corruption-repair
   instruction, excludes the rejected draft, and contains no result from a
   rejected call.
3. Supply a clean request that invokes a real built-in tool; verify its actual
   result before emitting a terminal response.
4. Repeat the corruption on the repair request and assert typed terminal failure
   after exactly two model requests, with no tool effect.
5. Send a clean quoted-marker control and an ordinary Python syntax-error case;
   verify literal preservation and normal syntax correction respectively.
6. Check the live metric increments, structured warning signatures, authenticated
   gateway response, and `mt-admin status goal34repl` health.

The tracker incident itself is nondeterministic; deterministic live replay proves
handling of those payloads, not the historical provider-side cause.
