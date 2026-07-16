# Engine V2 Local Compatibility Completion Design

**Status:** Approved for implementation on 2026-07-15.

## Context

Engine LLM Streaming Phase 5 is implemented on `master`. The channel-neutral
routing, streaming, terminal delivery, gate, authentication, and scoped control
work from commits `8044774`, `12a6a74`, `88a78fd`, and `7e2573d` is present in
the current tree. The authoritative Phase 5 procedure remains
`docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-5.md`.

The remaining work has two different failure domains:

1. local automated compatibility proof; and
2. staged validation on a disposable multi-tenant deployment.

This design covers the first domain only. A separate design will cover tenant
provisioning and live channel rollout after the local gates pass.

## Goal

Provide deterministic, hermetic evidence that Engine V2 works through the real
host boundaries for Phase 5 channel behavior, MCP tools, WASM tools, skills,
TensorZero-shaped streaming, and the browser gateway.

The resulting commit must be suitable for deployment to a fresh validation
tenant without relying on external credentials, public network services,
Docker, PostgreSQL, or an existing tenant.

## Non-Goals

- Do not provision, modify, restart, or test a live tenant in this subproject.
- Do not modify `ENGINE_V2` defaults or broaden `ENGINE_V2_CHANNELS`.
- Do not add incremental token delivery to WASM channels.
- Do not add user-visible tool-argument streaming.
- Do not change the channel WIT, database schema, TensorZero configuration, or
  extension wire protocols.
- Do not refactor the production engine, bridge, gateway, extension manager, or
  test framework merely to make tests aesthetically uniform.
- Do not use live API keys, OAuth credentials, public services, or secret-bearing
  tenant configuration.
- Freeze unrelated MCP feature and lifecycle development during this work. The
  MCP component may change tests and test support; a production MCP correction
  requires a reproducible Engine V2 RED and separate explicit maintainer
  approval before editing production MCP code.

## Design Principles

1. **Real boundary, hermetic dependency.** Exercise production adapters and
   registries against loopback fixtures or existing test WASM components.
2. **Focused targets.** Keep unrelated compatibility domains in separate test
   binaries or browser scenarios so failures identify the broken boundary.
3. **Test-first correction.** Production behavior changes only when a new
   regression test fails against a confirmed defect.
4. **Fail closed.** Missing artifacts, malformed protocol responses, leaked
   credentials, duplicate delivery, or incomplete stream termination fail the
   test explicitly.
5. **No global-state leakage.** Environment and Engine V2 singleton changes are
   serialized, restored, and reset between cases.

## Component 1: Phase 5 Plan-Fidelity Regressions

### WASM StreamChunk No-Op

Strengthen `channels::wasm::wrapper::tests::test_stream_chunk_is_noop` so it:

1. establishes a pending synchronous response waiter;
2. sends three `StatusUpdate::StreamChunk` values;
3. proves no chunk consumes or completes the waiter;
4. proves no typing task starts; and
5. proves a later terminal `respond()` completes the waiter exactly once.

This locks the intended contract: WASM channels receive final responses but do
not convert provider chunks into protocol messages.

### Original Response Metadata

Add a focused wrapper regression proving terminal serialization uses the
original `IncomingMessage.metadata`, not independently supplied response
metadata. Use distinct sentinels in both locations so the assertion cannot pass
accidentally.

### DarkIRC and WeeChat Scope Isolation

Add explicit bridge tests for:

- `darkirc:dm:alice` versus `darkirc:dm:bob`; and
- `weechat:dm:libera:alice` versus
  `weechat:group:libera:#lunarwing`.

For each pair, prove conversation lookup, approval matching, authentication
matching, and active-thread interruption remain scoped. A request ID from one
scope must not resolve or stop the other.

## Component 2: Engine V2 MCP Compatibility

Create a focused Rust integration target using the real MCP client,
`ToolRegistry`, `EffectBridgeAdapter`, and Engine V2 execution path.

A process-local loopback fixture implements only the MCP and OAuth endpoints
required by the test:

- initialization;
- tool discovery;
- one deterministic tool call;
- unauthenticated activation response;
- protected-resource and authorization metadata;
- token exchange; and
- authenticated retry.

The target must prove:

1. a discovered MCP tool becomes an Engine V2 action;
2. invoking it executes the actual MCP transport path;
3. authentication becomes a typed Engine V2 authentication gate;
4. credential resolution resumes the same conversation without storing the
   credential as user or assistant history;
5. the approved/authenticated action executes once; and
6. one terminal result is delivered and persisted.

The fixture binds only to `127.0.0.1:0`, uses synthetic credentials, has bounded
startup/request timeouts, and is stopped through RAII cleanup.

## Component 3: Engine V2 WASM Tool Compatibility

Create a focused Rust integration target that loads a small test-only WASM
component through `WasmToolRuntime` and registers it through the production
`ToolRegistry` path. The fixture lives under the integration-test fixture tree,
uses the repository's current tool WIT, and exposes deterministic actions for a
successful echo plus one denied capability attempt. It is built through a
dedicated fixture command and is never installed or catalogued as a production
extension.

The target must prove:

1. the WASM tool becomes an Engine V2 action;
2. an LLM action call reaches the Wasmtime component through
   `EffectBridgeAdapter`;
3. declared capabilities are enforced;
4. tool output passes through the normal safety/sanitization boundary;
5. the action result is tied to the original call ID; and
6. the engine emits one terminal response after the tool result.

The test must use replayed or loopback HTTP data where network capability is
needed. It must not require a public endpoint or real secret.

## Component 4: Engine V2 Skill Compatibility

Create a focused Rust integration target using a temporary workspace and a real
`SKILL.md` fixture with unique activation text.

The target must prove:

1. the skill is discovered by the existing v1 registry;
2. migration creates the expected Engine V2 `MemoryDoc` and metadata;
3. migration is idempotent by content hash;
4. deterministic Engine V2 selection activates the matching skill; and
5. a unique skill instruction reaches the model context for the matching turn
   but not a non-matching turn.

The test must exercise the active orchestrator selection boundary rather than
testing only metadata conversion helpers. Self-improvement, sharing, pruning,
and proposal approval remain covered by their existing focused tests and are not
expanded here.

## Component 5: TensorZero-Shaped End-to-End Streaming

Create a focused Rust integration target that reuses or extracts the existing
TensorZero `2026.3.2` loopback SSE fixture and runs it through:

```text
TensorZero-shaped SSE
  -> RigAdapter
  -> production LLM decorator boundary needed by the fixture
  -> LlmBridgeAdapter
  -> Engine V2 execution
  -> ResponseDelta events
  -> channel status
  -> one terminal response
```

Cover two cases:

1. fragmented text plus usage and `[DONE]`; and
2. fragmented tool calls with stable ordering and a complete action execution.

Assert exact chunk order, exactly one terminal response, terminal-only usage,
no duplicate first chunk, and strict failure on mid-stream protocol errors or
EOF before the terminal event.

This does not change TensorZero configuration and does not require a live
TensorZero deployment.

## Component 6: Gateway Browser Compatibility

Extend the existing Python/Playwright E2E harness with an Engine V2 server
fixture rather than changing the default legacy fixture. The new fixture sets
`ENGINE_V2=true`, retains the loopback mock LLM and libSQL database, and uses a
separate temporary home/database to prevent cross-mode state leakage.

The E2E harness must accept an explicit `LUNARWING_E2E_BINARY` path and must not
invoke `cargo build` when that override is supplied. Engine V2 browser gates use
the release binary built once for the immediately following disposable-tenant
deployment. Missing or stale explicit binaries fail with actionable guidance;
the completion workflow never triggers a full debug build.

Add a focused browser scenario proving:

1. ordered stream chunks render incrementally;
2. the terminal response finalizes rather than duplicates the streamed message;
3. an approval card resolves and produces one final result;
4. an authentication prompt resolves without rendering or retaining the token;
5. interrupt acknowledgement appears promptly;
6. the cancelled turn produces no terminal assistant response; and
7. a later message in the same thread succeeds.

Prefer DOM and wire-visible assertions. Do not add screenshot/pixel assertions
unless a production UI defect is found.

## Shared Test Support

Shared helpers may be added only when at least two targets require the same
behavior. Suitable shared concerns are:

- scoped Engine V2 environment setup and restoration;
- engine singleton reset;
- loopback server lifecycle;
- deterministic streamed LLM responses;
- action-result and history assertions; and
- unique temporary workspace/database creation.

Do not move domain-specific assertions into a generic harness. Each target owns
its compatibility contract.

## Data and Control Flow

Every integration target follows the same outer flow:

```text
real host registration boundary
  -> ToolRegistry / SkillRegistry / RigAdapter
  -> Engine V2 bridge adapter
  -> engine thread and action execution
  -> gate or action result
  -> channel-neutral status and terminal delivery
  -> compatibility history assertions
```

Authentication and approval fixtures use synthetic values. Tests assert the
values are absent from v1 history, Engine V2 messages, captured channel output,
and failure diagnostics.

## Error Handling and Cleanup

- Loopback servers bind to port zero and report the selected port through a
  bounded readiness channel.
- All server, process, stream, and browser waits use explicit timeouts.
- Drop/cleanup guards stop child tasks and restore environment variables even
  after assertion failure.
- Engine singleton state is reset between serialized cases.
- No test retries after a visible chunk or executed side effect.
- Unexpected duplicate tool calls, statuses, responses, or history rows fail
  with counts and non-secret identifiers.
- Credential sentinels must never appear in assertion messages.

## Test and Verification Sequence

Use test-driven development for each missing contract:

1. write the narrow regression or integration case;
2. run it and record the expected RED reason;
3. make the minimum test-support or production correction;
4. rerun the target GREEN;
5. run the adjacent subsystem suite; and
6. run the complete local gate after all targets pass.

All Cargo commands run from `ic/` with `taskset -c 0-5` and `-j6`. Use
`cargo check`, never a full debug build for Rust compile verification. Build one
release binary under tmux only when the local targets are green and the artifact
will immediately feed the browser gate and subsequent disposable-tenant
deployment. Pass that artifact through `LUNARWING_E2E_BINARY`; do not allow the
browser harness to fall back to a debug build in the completion workflow.

The final local gate includes:

- formatting;
- default, PostgreSQL-only, libSQL-only, and all-feature compile checks;
- `lunarwing_engine` tests;
- bridge, effect-adapter, gate, WASM wrapper, and channel crate tests;
- the existing Phase 4 interrupt and Phase 5 delivery integration targets;
- all new focused compatibility targets;
- Engine V2 browser E2E scenarios;
- default and all-feature Clippy with warnings denied;
- WIT unchanged proof;
- source guards for direct terminal SSE and centralized channel policy; and
- `git diff --check`.

## Completion Criteria

This local-automation subproject is complete only when:

1. all three Phase 5 plan-fidelity gaps have direct regressions;
2. real MCP, WASM, skill, TensorZero-shaped, and browser boundaries pass through
   Engine V2 in hermetic tests;
3. tests expose no credential or cross-scope leakage;
4. no test relies on public network access or live credentials;
5. every required local gate passes under the six-thread constraint;
6. documentation accurately distinguishes local proof from pending live tenant
   validation; and
7. the resulting commit is identified as the input to the separate disposable
   tenant rollout plan.

## Rollback and Risk

The intended changes are test and test-support additions. If a regression
requires production correction, keep that correction minimal and pair it with
the failing test that exposed it.

No live rollback is needed in this subproject. Source rollback consists of
reverting the focused test/support change and any inseparable production fix.
The subsequent tenant rollout retains the existing per-channel
`ENGINE_V2_CHANNELS` rollback and broader `ENGINE_V2=false` rollback.
