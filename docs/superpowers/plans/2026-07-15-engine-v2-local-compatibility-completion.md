# Engine V2 Local Compatibility Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining Phase 5 regression gaps and prove Engine V2 compatibility through real, hermetic MCP, WASM, skill, TensorZero-shaped streaming, and browser-gateway boundaries before live tenant rollout.

**Architecture:** Preserve the existing Phase 5 implementation and add focused compatibility targets around production adapters and registries. Each target uses libSQL, loopback fixtures, synthetic credentials, serialized Engine V2 environment state, and the normal channel/history boundary. Production changes are permitted only when a new RED test demonstrates a real defect.

**Tech Stack:** Rust 2024, Tokio, Axum, libSQL, rig-core 0.40, LunarWing Engine V2 bridge adapters, Wasmtime component tools, MCP HTTP/OAuth, Python 3.11+, pytest, Playwright, existing LunarWing test support.

## Global Constraints

- Run every Cargo command from `ic/` with `taskset -c 0-5` and `-j6`.
- Use `cargo check` for compile verification. Never run a full debug `cargo build`.
- Long-running commands, WASM component builds, and the one release build run under tmux.
- Use no public network service, real credential, Docker dependency, PostgreSQL dependency, or live tenant in this plan.
- Bind protocol fixtures only to `127.0.0.1:0` and give every startup, request, stream, and shutdown an explicit timeout.
- Restore environment variables and call `lunarwing::bridge::reset_engine_state().await` between serialized Engine V2 cases.
- Do not change channel WIT, database schema, TensorZero configuration, `ENGINE_V2` defaults, or `ENGINE_V2_CHANNELS` rollout policy.
- WASM channels continue ignoring `StatusUpdate::StreamChunk`; tool-argument UI streaming remains deferred.
- Freeze unrelated MCP feature and lifecycle development. Task 3 may modify MCP tests and test support; any production MCP correction requires a reproducible Engine V2 RED and separate explicit maintainer approval before production MCP files are edited.
- Do not commit unless the maintainer explicitly requests commits. Commit commands below are optional checkpoints for use only after that authorization.
- Authoritative design: `docs/superpowers/specs/2026-07-15-engine-v2-local-compatibility-completion-design.md`.
- Authoritative existing Phase 5 procedure: `docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-5.md`.

---

## File Map

### Existing files to modify

- `ic/src/channels/wasm/wrapper.rs` — strengthen StreamChunk and terminal metadata regressions in its private-field-aware unit-test module.
- `ic/src/bridge/router.rs` — add explicit DarkIRC and WeeChat control-scope regressions in the existing router test module.
- `ic/tests/support/mod.rs` — export the Engine V2 environment guard.
- `ic/tests/support/test_rig.rs` — seed real SKILL.md fixtures before `AppBuilder::build_all()`.
- `ic/Cargo.toml` — register four focused Rust integration targets behind `libsql,integration`.
- `ic/tests/e2e/conftest.py` — add a mandatory explicit-binary Engine V2 server fixture without altering the legacy fixture.
- `ic/tests/e2e/mock_llm.py` — add deterministic Engine V2 stream/tool/auth/interrupt scripts used by the browser scenario.
- `docs/architecture/ENGINE-V2.md`, `docs/proposals/ENGINE_LLM_STREAMING.md`, `ic/FEATURE_PARITY.md` — record local proof separately from pending tenant rollout.

### New files

- `ic/tests/support/engine_v2_env.rs` — synchronous RAII environment restoration plus explicit async engine reset.
- `ic/tests/engine_v2_mcp_compatibility.rs` — real MCP client and Engine V2 action compatibility.
- `ic/tests/engine_v2_wasm_tool.rs` — real Wasmtime component execution through Engine V2.
- `ic/tests/engine_v2_skill_selection.rs` — real skill discovery, migration, selection, and context injection.
- `ic/tests/engine_v2_tensorzero_streaming.rs` — local TensorZero-shaped SSE through RigAdapter and Engine V2.
- `ic/tests/fixtures/test-echo-tool/Cargo.toml`
- `ic/tests/fixtures/test-echo-tool/src/lib.rs`
- `ic/tests/fixtures/test-echo-tool/wit/tool.wit`
- `ic/tests/fixtures/test-echo-tool/test-echo-tool.capabilities.json`
- `ic/tests/e2e/scenarios/test_engine_v2.py`

---

### Task 1: Add Serialized Engine V2 Test Environment Support

**Files:**
- Create: `ic/tests/support/engine_v2_env.rs`
- Modify: `ic/tests/support/mod.rs`
- Modify: `ic/tests/engine_v2_channel_delivery.rs`

**Interfaces:**
- Produces: `EngineV2EnvGuard::enable(channels: Option<&str>) -> EngineV2EnvGuard`
- Produces: `EngineV2EnvGuard::set_channels(&self, channels: Option<&str>)`
- Produces: `EngineV2EnvGuard::cleanup(self) -> impl Future<Output = ()>`
- `Drop` restores only `ENGINE_V2` and `ENGINE_V2_CHANNELS`; it performs no async work.

- [ ] **Step 1: Write environment restoration tests**

In `engine_v2_env.rs`, add one serialized Tokio test that saves sentinel values, calls `EngineV2EnvGuard::enable(Some("xmpp"))`, checks both active values, calls `cleanup().await`, and checks the sentinels were restored. Use a process-local `static Mutex<()>` in the test module so environment mutation cannot overlap.

- [ ] **Step 2: Run the new support test and confirm RED**

```bash
taskset -c 0-5 cargo test -j6 --test engine_v2_channel_delivery \
  engine_v2_env -- --test-threads=1 --nocapture
```

Expected: compilation fails because `support::engine_v2_env` does not exist.

- [ ] **Step 3: Implement the guard by extracting the existing proven pattern**

Move the `EngineEnvGuard` and `restore_env` behavior currently in
`ic/tests/engine_v2_channel_delivery.rs:521-571` into the new support module.
Keep the Rust 2024 safety comments explaining that each consuming integration
binary is serialized. Implement explicit cleanup as:

```rust
pub async fn cleanup(self) {
    lunarwing::bridge::reset_engine_state().await;
    drop(self);
}
```

Do not call a runtime from `Drop`.

- [ ] **Step 4: Switch the existing Phase 5 matrix to the shared guard**

Replace the local guard type with `support::engine_v2_env::EngineV2EnvGuard`.
Preserve all existing explicit `reset_engine_state().await` calls until the
matrix passes; remove only exact duplicates that immediately precede
`guard.cleanup().await`.

- [ ] **Step 5: Run the existing Phase 5 matrix**

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_channel_delivery \
  -- --test-threads=1 --nocapture
```

Expected: `engine_v2_channel_delivery_matrix` passes and environment state is
restored after the test binary exits.

- [ ] **Step 6: Optional commit checkpoint**

```bash
git add ic/tests/support/engine_v2_env.rs ic/tests/support/mod.rs \
  ic/tests/engine_v2_channel_delivery.rs
git commit -m "test(support): centralize serialized Engine V2 environment setup"
```

Run only after explicit commit authorization.

---

### Task 2: Close the Three Phase 5 Plan-Fidelity Gaps

**Files:**
- Modify: `ic/src/channels/wasm/wrapper.rs`
- Modify: `ic/src/bridge/router.rs`

**Interfaces:**
- Consumes private test-visible fields `WasmChannel::pending_responses`,
  `WasmChannel::typing_task`, and `WasmChannel::last_broadcast_metadata`.
- Consumes existing router helpers `has_matching_engine_approval()` and
  `has_active_engine_thread()`.
- Produces no production API.

- [ ] **Step 1: Strengthen `test_stream_chunk_is_noop`**

Inside the existing wrapper test module:

1. create and start `create_test_channel()`;
2. create an `IncomingMessage`, retain its `id`, and insert a
   `oneshot::Sender<String>` into `channel.pending_responses` under that ID;
3. send three distinct `StatusUpdate::StreamChunk` values;
4. assert `typing_task` remains `None`;
5. assert the receiver does not complete during a 50 ms timeout;
6. call `respond()` with the same message ID;
7. assert the receiver completes within one second with the terminal text; and
8. assert the pending map no longer contains the ID.

Keep the existing test name so the Phase 5 verification command remains stable.

- [ ] **Step 2: Run the StreamChunk regression**

```bash
taskset -c 0-5 cargo test -j6 --lib \
  channels::wasm::wrapper::tests::test_stream_chunk_is_noop \
  -- --exact --nocapture
```

Expected: GREEN without a production change. If RED, change only the
`StatusUpdate::StreamChunk(_) => {}` handling required to restore no-op behavior.

- [ ] **Step 3: Add terminal metadata source regression**

Add `test_respond_uses_original_incoming_metadata`. Give the incoming message
metadata `{"source":"incoming","chat_id":42}` and the outgoing response
metadata `{"source":"response","chat_id":99}`. Call `respond()`, read the
metadata captured by the test guest using the existing wrapper test callback
state, and assert only the incoming sentinel and route are serialized to
`on_respond`.

- [ ] **Step 4: Run the metadata regression**

```bash
taskset -c 0-5 cargo test -j6 --lib \
  channels::wasm::wrapper::tests::test_respond_uses_original_incoming_metadata \
  -- --exact --nocapture
```

Expected: GREEN. If RED, minimally correct `WasmChannel::respond()` so its WIT
response metadata is derived from `msg.metadata`, preserving all existing
response content and attachment behavior.

- [ ] **Step 5: Add explicit DarkIRC and WeeChat scope tests**

In the router test module, add:

- `darkirc_dm_scopes_do_not_cross_match_controls`
- `weechat_dm_and_group_scopes_do_not_cross_match_controls`

For each test, initialize two conversations for the same owner and channel,
insert distinct pending approval and authentication gates, and start one active
thread per scope. Assert an exact request ID and a simple approval from scope A
cannot resolve scope B, an auth token from scope A leaves scope B pending, and
`has_active_engine_thread()` changes only for the interrupted scope. Reuse the
existing in-module gate and conversation setup helpers rather than adding a
production constructor.

- [ ] **Step 6: Run scoped control tests**

```bash
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::darkirc_dm_scopes_do_not_cross_match_controls \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::weechat_dm_and_group_scopes_do_not_cross_match_controls \
  -- --exact --nocapture
```

Expected: both pass. A failure permits a minimal correction only in the
conversation-scoped matcher or active-thread lookup demonstrated by the RED
case.

- [ ] **Step 7: Run adjacent suites**

```bash
taskset -c 0-5 cargo test -j6 --lib channels::wasm::wrapper::tests \
  -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests \
  -- --test-threads=6
```

- [ ] **Step 8: Optional commit checkpoints**

Use one commit for the two inseparable wrapper regressions and one for the
router scope regressions, only after explicit authorization.

---

### Task 3: Prove Real MCP Tool Execution and Authentication Through Engine V2

**Files:**
- Create: `ic/tests/engine_v2_mcp_compatibility.rs`
- Modify: `ic/tests/support/mock_mcp_server.rs`
- Modify: `ic/tests/support/test_rig.rs`
- Modify: `ic/Cargo.toml`

**Interfaces:**
- Consumes `start_mock_mcp_server(Vec<MockToolResponse>) -> MockMcpServer`.
- Consumes `McpClient::new_authenticated(McpServerConfig, Arc<McpSessionManager>, Arc<dyn SecretsStore + Send + Sync>, user_id)`.
- Consumes `McpClient::create_tools() -> Result<Vec<Arc<dyn Tool>>, ToolError>`.
- Produces: `TestRigBuilder::with_mcp_server_config(config: McpServerConfig) -> Self`.
- Produces test-only mock call counts and an authorization toggle on
  `MockMcpServer`; neither is exported by production code.

- [ ] **Step 1: Add pre-AppBuilder MCP configuration seeding**

Add `mcp_server_configs: Vec<McpServerConfig>` to `TestRigBuilder` and a
`with_mcp_server_config()` method following the existing `with_extra_tools()`
builder pattern. During `build()`, after `Config::for_testing()` provides the
owner ID and before `AppBuilder::build_all()`, create a default
`McpServersFile`, `upsert()` each supplied config, and call
`save_mcp_servers_to_db(db.as_ref(), &config.owner_id, &servers)`. This makes the
real ExtensionManager load the installed configuration and permits the real
`tool_activate` path to produce an authentication gate.

- [ ] **Step 2: Extend the mock MCP server with observable state**

Add an `Arc<MockMcpObservations>` containing atomic initialize/list/call counts
and a token-required flag. Expose read-only methods on `MockMcpServer`:

```rust
pub fn initialize_count(&self) -> usize;
pub fn tools_list_count(&self) -> usize;
pub fn tool_call_count(&self, name: &str) -> usize;
pub fn require_token(&self, required: bool);
```

Keep the accepted synthetic bearer token `mock-access-token` and existing OAuth
metadata endpoints.

- [ ] **Step 3: Add a synthetic MCP token SecretsStore in the integration test**

Implement a test-only `SecretsStore` whose `get_decrypted()` returns
`mock-access-token`, whose `exists()` returns true, and whose unrelated mutation
methods return explicit test errors rather than `unimplemented!()`. Do not put
the token in panic or assertion messages.

- [ ] **Step 4: Write authenticated real-transport RED test**

Start the mock server with one `mock_search` response. Build
`McpServerConfig::new("mock-mcp", mock.mcp_url())`, an
`Arc<McpSessionManager>`, the synthetic store, and a real authenticated
`McpClient`. Call `create_tools()`, register the returned tools with
`TestRigBuilder::with_extra_tools()`, and use an inline deterministic
`LlmProvider` that emits exactly one `mock_search` call followed by terminal
text after receiving the result.

Assert:

- the engine advertises the MCP action;
- initialize and tools/list occur once;
- `mock_search` executes once over HTTP;
- the action result retains the original call ID;
- one assistant response is delivered and persisted; and
- no synthetic token appears in engine messages, compatibility history, channel
  output, or captured diagnostics.

- [ ] **Step 5: Add authentication pause/resume RED test**

Configure the server to reject the initial unauthenticated activation. Seed its
`McpServerConfig` with `with_mcp_server_config()`, then drive the real
`tool_activate` action through Engine V2 using the test rig's existing extension
tools. Assert one
`StatusUpdate::AuthRequired`, no terminal duplicate, and no credential history.
Store the synthetic token through the existing OAuth/session path, resume the
same conversation, and assert the discovered MCP action executes once and
produces one terminal response.

- [ ] **Step 6: Run the MCP target RED, then apply only demonstrated fixes**

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_mcp_compatibility \
  -- --test-threads=1 --nocapture
```

Expected initial RED: the new mock observations and test target are absent.
After implementing support, both cases pass. A production fix is allowed only
if the real transport or typed auth gate still fails.

- [ ] **Step 7: Run adjacent MCP and effect-adapter tests**

```bash
taskset -c 0-5 cargo test -j6 --lib tools::mcp:: -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::effect_adapter::tests \
  -- --test-threads=6
```

- [ ] **Step 8: Optional commit checkpoint**

```bash
git add ic/tests/engine_v2_mcp_compatibility.rs \
  ic/tests/support/mock_mcp_server.rs ic/tests/support/test_rig.rs ic/Cargo.toml
git commit -m "test(engine): exercise MCP transport and auth through Engine V2"
```

Run only after explicit commit authorization.

---

### Task 4: Prove a Real WASM Tool Through Engine V2

**Files:**
- Create: `ic/tests/fixtures/test-echo-tool/Cargo.toml`
- Create: `ic/tests/fixtures/test-echo-tool/src/lib.rs`
- Create: `ic/tests/fixtures/test-echo-tool/wit/tool.wit`
- Create: `ic/tests/fixtures/test-echo-tool/test-echo-tool.capabilities.json`
- Create: `ic/tests/engine_v2_wasm_tool.rs`
- Modify: `ic/Cargo.toml`

**Interfaces:**
- Consumes `TestRigBuilder::with_wasm_tool(name, wasm_path, capabilities_path)`.
- Consumes the current `ic/wit/tool.wit` copied unchanged into the fixture.
- The fixture exports deterministic `echo` and `denied_http_probe` actions only.

- [ ] **Step 1: Create the test-only component**

Use an isolated Cargo workspace with `crate-type = ["cdylib"]`, `wit-bindgen =
"0.41"`, and a local `wit/tool.wit` copy. Implement `echo` to return
`{"source":"engine-v2-test-wasm","echo":<input>}`. Implement
`denied_http_probe` by invoking the WIT HTTP host import while the capability
file contains an empty HTTP allowlist. Do not add the fixture to the extension
registry.

- [ ] **Step 2: Build the fixture under tmux**

```bash
tmux new-session -d -s engine-v2-test-wasm \
  "cd /var/lib/paseo/.paseo/worktrees/3ckmjlbe/noisy-duck/ic && \
   taskset -c 0-5 cargo component build -j6 --release --target wasm32-wasip2 \
   --manifest-path tests/fixtures/test-echo-tool/Cargo.toml \
   2>&1 | tee /tmp/engine-v2-test-wasm.log"
```

Expected artifact:
`ic/tests/fixtures/test-echo-tool/target/wasm32-wasip2/release/test_echo_tool.wasm`.
The integration target must panic with this exact build instruction if the
artifact is absent; it must never skip or return success.

- [ ] **Step 3: Write successful execution RED test**

Use `EngineV2EnvGuard`, `TestRigBuilder::with_wasm_tool()`, and an inline LLM
that requests `echo` with a fixed call ID. Assert the action is advertised,
Wasmtime executes the component, the result contains the fixture source marker,
the recorded action result retains the call ID, and exactly one terminal
assistant response is delivered and persisted.

- [ ] **Step 4: Write denied-capability RED test**

Request `denied_http_probe`. Assert the host rejects the undeclared endpoint,
the error is returned as one structured tool result, no external connection is
attempted, no credential-like sentinel appears in output, and the engine either
continues to one terminal explanation or fails once without duplicate delivery.

- [ ] **Step 5: Run target and adjacent WASM tests**

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_wasm_tool \
  -- --test-threads=1 --nocapture
taskset -c 0-5 cargo test -j6 --lib tools::wasm:: -- --test-threads=6
```

Expected: both focused cases and existing WASM host tests pass. Make no
production change unless the RED case identifies a real adapter/runtime defect.

- [ ] **Step 6: Prove the production WIT is unchanged**

```bash
git -C .. diff --exit-code -- ic/wit/tool.wit
```

- [ ] **Step 7: Optional commit checkpoint**

Commit the fixture and its integration target together only after explicit
authorization.

---

### Task 5: Prove Skill Discovery, Migration, Selection, and Context Injection

**Files:**
- Create: `ic/tests/engine_v2_skill_selection.rs`
- Modify: `ic/tests/support/test_rig.rs`
- Modify: `ic/Cargo.toml`

**Interfaces:**
- Produces: `TestRigBuilder::with_seeded_skill(filename: impl Into<String>, content: impl Into<String>) -> Self`.
- Adds builder field: `skills_to_seed: Vec<(String, String)>`.
- Consumes existing `with_skills()`, `captured_llm_requests()`, `workspace()`,
  captured statuses, and Engine V2 memory APIs.

- [ ] **Step 1: Write builder-support RED test**

Add `skills_to_seed` to `TestRigBuilder`. During `build()`, after creating
`skills_dir` and before `Config::for_testing()`/`AppBuilder::build_all()`, validate
that each filename is a single relative component, then write the supplied
content into the real skills directory. Reject absolute paths, `..`, and path
separators with a test panic that prints only the invalid filename.

- [ ] **Step 2: Add a real SKILL.md fixture through the builder**

Use frontmatter with name `engine-v2-selection-proof`, a unique activation token
`lunarwing-engine-v2-skill-proof`, and body instruction
`Include SKILL_CONTEXT_PROOF in the model-visible guidance.`.

- [ ] **Step 3: Write matching-turn RED test**

Build a skill-enabled gateway rig under Engine V2. Send the unique activation
token. Assert:

- one migrated `DocType::Skill` exists with the expected content hash;
- one `StatusUpdate::SkillActivated` names the fixture;
- captured model input contains `SKILL_CONTEXT_PROOF`; and
- one terminal response is delivered.

- [ ] **Step 4: Write non-matching and idempotence RED tests**

Send a non-matching message in a reset conversation and assert no activation and
no marker in model input. Run migration twice against the same store and assert
the second run adds zero documents and leaves the document count unchanged.

- [ ] **Step 5: Run target and adjacent skill tests**

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_skill_selection \
  -- --test-threads=1 --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_skills -- --test-threads=6
```

Expected: matching, non-matching, and idempotence cases pass. Restrict any
production correction to the migration, orchestrator selection, or context
injection path proven faulty by RED.

- [ ] **Step 6: Optional commit checkpoint**

Commit `test_rig.rs`, the integration target, and its Cargo registration as one
inseparable test-support unit only after explicit authorization.

---

### Task 6: Prove TensorZero-Shaped Streaming Through the Real Adapter Chain

**Files:**
- Create: `ic/tests/engine_v2_tensorzero_streaming.rs`
- Modify: `ic/Cargo.toml`

**Interfaces:**
- Defines a target-local Axum SSE fixture bound to `127.0.0.1:0`.
- Constructs rig-core exactly as production does:
  `ReqwestClient::builder().timeout(...)`,
  `openai::Client::builder().api_key("test-key").base_url(...)`,
  `.completions_api().completion_model("test-model")`, and
  `RigAdapter::new(model, "test-model")`.
- Consumes `LlmBridgeAdapter` and the normal Engine V2/TestRig delivery path.

- [ ] **Step 1: Implement target-local SSE scripts**

Provide scripts for:

1. three text deltas, terminal usage, and `[DONE]`;
2. fragmented tool ID/name/arguments with stable index and `[DONE]`;
3. a JSON mid-stream error; and
4. EOF before Rig's terminal event.

Expose request count and captured request body so the test can assert
`stream=true` and `stream_options.include_usage=true`.

- [ ] **Step 2: Write text-stream RED test**

Pass the real `RigAdapter` into a gateway TestRig. Assert exact chunk order,
three `StreamChunk` statuses before one response, concatenated terminal text,
one persisted assistant row, terminal-only usage, and one HTTP request.

- [ ] **Step 3: Write fragmented-tool RED test**

Register a deterministic Rust echo tool solely as the action endpoint. Emit
fragmented TensorZero tool deltas and assert one reconstructed action with the
original provider call ID, one execution, one structured result, and one
terminal response.

- [ ] **Step 4: Write strict error RED tests**

For the JSON mid-stream error and premature EOF, assert no synthetic `Done`, no
cache/recording commit, no persisted partial assistant response, and at most one
user-visible error response.

- [ ] **Step 5: Run target and RigAdapter unit tests**

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_tensorzero_streaming \
  -- --test-threads=1 --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::rig_adapter::tests \
  -- --test-threads=6
```

Expected: all four cases and existing TensorZero `2026.3.2` fixtures pass.

- [ ] **Step 6: Optional commit checkpoint**

Commit the target and Cargo registration only after explicit authorization.

---

### Task 7: Add Engine V2 Browser E2E Without Debug Builds

**Files:**
- Modify: `ic/tests/e2e/conftest.py`
- Modify: `ic/tests/e2e/mock_llm.py`
- Create: `ic/tests/e2e/scenarios/test_engine_v2.py`

**Interfaces:**
- Produces session fixture `engine_v2_binary()` requiring
  `LUNARWING_E2E_BINARY`.
- Produces session fixture `engine_v2_server()` with independent temporary home,
  libSQL database, gateway/http ports, and `ENGINE_V2=true`.
- Produces function fixture `engine_v2_page()`.
- Does not alter the existing legacy `lunarwing_binary` or `lunarwing_server` fixtures.

- [ ] **Step 1: Add mandatory explicit binary fixture**

Resolve `LUNARWING_E2E_BINARY` to an absolute path, fail if unset/nonexistent/not
executable, and return it. The failure message instructs the operator to build
the release binary under tmux. Do not call Cargo from this fixture.

- [ ] **Step 2: Add isolated Engine V2 server fixture**

Factor only the environment-dictionary construction shared safely with the
legacy fixture. Use separate temporary directories and ports, set
`ENGINE_V2=true`, leave `ENGINE_V2_CHANNELS` unset, retain the loopback mock LLM,
and reuse the existing bounded startup/graceful-shutdown helpers.

- [ ] **Step 3: Add deterministic mock scripts**

Add prompt sentinels for streamed text, approval-required tool execution,
authentication pause/resume, long streaming interruption, and post-interrupt
recovery. Each mock path records call counts and never logs the synthetic auth
token.

- [ ] **Step 4: Write browser RED scenarios**

In `test_engine_v2.py`, assert:

- partial text appears before terminal completion;
- terminal completion leaves exactly one assistant message bubble;
- approval card appears once and approval produces one final result;
- auth prompt accepts a synthetic token while the token never appears in DOM;
- interrupt acknowledgement appears within two seconds;
- no cancelled terminal assistant response appears after a quiescence window;
  and
- a later same-thread message completes normally.

Use DOM and captured SSE/network events, not screenshots.

- [ ] **Step 5: Build one deployable release binary under tmux**

```bash
tmux new-session -d -s engine-v2-compat-release \
  "cd /var/lib/paseo/.paseo/worktrees/3ckmjlbe/noisy-duck/ic && \
   taskset -c 0-5 cargo build --release -j6 --bin lunarwing \
   2>&1 | tee /tmp/engine-v2-compat-release.log"
```

This is the only native build command in the plan. Retain the artifact for the
immediately following disposable-tenant rollout subproject.

- [ ] **Step 6: Run Engine V2 browser scenarios**

```bash
LUNARWING_E2E_BINARY=/var/lib/paseo/.paseo/worktrees/3ckmjlbe/noisy-duck/ic/target/release/lunarwing \
  pytest tests/e2e/scenarios/test_engine_v2.py -v
```

Expected: all Engine V2 scenarios pass without the harness invoking Cargo.

- [ ] **Step 7: Run Python syntax checks**

```bash
python -m py_compile tests/e2e/conftest.py tests/e2e/mock_llm.py \
  tests/e2e/scenarios/test_engine_v2.py
```

- [ ] **Step 8: Optional commit checkpoint**

Commit the fixture, mock scripts, and scenario together only after explicit
authorization.

---

### Task 8: Update Local-Proof Documentation

**Files:**
- Modify: `docs/architecture/ENGINE-V2.md`
- Modify: `docs/proposals/ENGINE_LLM_STREAMING.md`
- Modify: `ic/FEATURE_PARITY.md`
- Modify: `docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-5.md`

**Interfaces:**
- Produces no code.
- Records exact test commands/counts and the release binary commit only after
  the corresponding gates pass.

- [ ] **Step 1: Document the compatibility boundary**

State that local proof now covers real MCP transport/auth, real WASM execution,
skill migration/selection, TensorZero-shaped RigAdapter streaming, Phase 5
channels, and Engine V2 browser behavior. Preserve the distinction between
gateway incremental streaming and WASM final-response delivery.

- [ ] **Step 2: Preserve pending live status**

Keep Phase 5 live rollout open. State that XMPP/DarkIRC/WeeChat tenant validation
belongs to the separate disposable-tenant plan and that no local test proves a
deployed protocol bridge.

- [ ] **Step 3: Record sanitized local evidence**

Record command names, pass counts, target commit, and build status. Do not record
tokens, bearer headers, complete environment values, or secret fixture values.

- [ ] **Step 4: Optional commit checkpoint**

Commit documentation separately only after explicit authorization.

---

### Task 9: Run the Complete Local Gate

**Files:**
- Verify only; no planned edits.

**Interfaces:**
- Consumes all previous task outputs.
- Produces the exact commit/artifact input for the disposable-tenant rollout.

- [ ] **Step 1: Format and compile**

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo check -j6 --no-default-features --features postgres
taskset -c 0-5 cargo check -j6 --no-default-features --features libsql
taskset -c 0-5 cargo check -j6 --all-features
```

- [ ] **Step 2: Run engine, bridge, effect, gate, and WASM suites**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::effect_adapter::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib gate:: -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib channels::wasm::wrapper::tests -- --test-threads=6
```

- [ ] **Step 3: Run Phase 4, Phase 5, and new focused targets serially**

```bash
for target in \
  engine_v2_interrupt_ingress \
  engine_v2_channel_delivery \
  engine_v2_mcp_compatibility \
  engine_v2_wasm_tool \
  engine_v2_skill_selection \
  engine_v2_tensorzero_streaming
do
  taskset -c 0-5 cargo test -j6 --no-default-features \
    --features libsql,integration --test "$target" -- --test-threads=1 --nocapture
done
```

- [ ] **Step 4: Run actual channel crate tests**

```bash
taskset -c 0-5 cargo test -j6 --manifest-path channels-src/xmpp/Cargo.toml
taskset -c 0-5 cargo test -j6 --manifest-path channels-src/darkirc/Cargo.toml
taskset -c 0-5 cargo test -j6 --manifest-path channels-src/weechat/Cargo.toml
```

- [ ] **Step 5: Run Engine V2 browser target with the retained release binary**

```bash
LUNARWING_E2E_BINARY=/var/lib/paseo/.paseo/worktrees/3ckmjlbe/noisy-duck/ic/target/release/lunarwing \
  pytest tests/e2e/scenarios/test_engine_v2.py -v
```

- [ ] **Step 6: Run zero-warning Clippy under tmux**

```bash
tmux new-session -d -s engine-v2-compat-clippy \
  "cd /var/lib/paseo/.paseo/worktrees/3ckmjlbe/noisy-duck/ic && \
   taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples -- -D warnings && \
   taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples \
     --all-features -- -D warnings \
   2>&1 | tee /tmp/engine-v2-compat-clippy.log"
```

- [ ] **Step 7: Prove ABI and source invariants**

From the repository root:

```bash
git diff --exit-code -- ic/wit/tool.wit ic/wit/channel.wit \
  ic/channels-src/*/channel.wit
git diff --check
rg -n "AppEvent::Response|ResponseDelta|StreamChunk|ENGINE_V2_CHANNELS" \
  ic/src/bridge/router.rs ic/src/agent/agent_loop.rs
```

Manually require that `AppEvent::Response` is absent from
`await_thread_outcome`, `ResponseDelta` has one channel-status route, and
`ENGINE_V2_CHANNELS` is read only by the bridge policy.

- [ ] **Step 8: Run repository safety checks**

```bash
bash scripts/pre-commit-safety.sh
bash scripts/check-boundaries.sh
```

- [ ] **Step 9: Record the handoff**

Record the clean commit hash, release binary path/hash, exact passed commands,
and any explicitly pre-existing unrelated failures. The next plan begins with a
fresh disposable tenant and may not modify antelope or barracuda.

---

## Execution Order and Parallelism

1. Execute Task 1 first because every focused integration target mutates Engine
   V2 process state.
2. Task 2 is independent after the current tree is confirmed clean.
3. Tasks 3, 4, 5, and 6 may be implemented in parallel in isolated worktrees,
   but their `Cargo.toml` edits must be integrated carefully and each final test
   binary runs serially.
4. Task 7 starts only after Tasks 3-6 are locally green because it exercises
   their user-visible composition.
5. Task 8 records evidence only after tests pass.
6. Task 9 is the mandatory final gate before drafting the disposable-tenant
   rollout plan.

## Completion Definition

The plan is complete only when every checkbox through Task 9 passes, no secret
or cross-scope leakage is observed, no live tenant was touched, no production
ABI/schema/default changed, and a release artifact from the verified commit is
ready for a separate fresh-tenant rollout.
