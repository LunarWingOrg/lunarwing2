# Software Builder System — Analysis Report

**Date:** 2026-07-23
**Analyst:** Automated deep-dive
**Codebase:** LunarWing v2 (`/home/openjaw/lunarwing2/ic`)
**Scope:** `src/tools/builder/`, `src/config/builder.rs`, `src/settings.rs`, `src/tools/registry.rs`, `src/app.rs`, `src/agent/{agent_loop,self_repair,reflex}.rs`, `src/bridge/effect_adapter.rs`

---

## 1. What the Software Builder Does

The Software Builder is an **LLM-driven code generation and compilation system** that lets the LunarWing agent create new software artifacts on demand. It is exposed to the LLM as the `build_software` tool.

### Capabilities

| Software Type | Languages | What It Produces |
|---|---|---|
| `wasm_tool` | Rust | WASM Component tool (`wasm32-wasip2` target via `cargo-component`) |
| `cli_binary` | Rust, Go, TS | Standalone command-line executables |
| `library` | Rust | Reusable crate |
| `script` | Python, Bash | Interpreted scripts |
| `web_service` | (declared, not fully wired) | HTTP API services |

### Build Loop Architecture

The builder runs a multi-phase iterative loop (`src/tools/builder/core.rs:1-23`):

1. **Analyze** — LLM parses a natural-language description into a structured `BuildRequirement`
2. **Scaffold** — Initial project files created via `write_file` tool
3. **Implement** — LLM writes code using `write_file` / `apply_patch`
4. **Build/Compile** — LLM invokes `shell` to run `cargo build`, `npm run build`, etc.
5. **Fix Errors** — Parse errors, LLM modifies code, retry
6. **Test** — Run tests, fix failures
7. **Validate** — For WASM tools, validate interface compliance (WasmValidator)
8. **Package** — Produce final artifact

### Two Consumers

The builder is consumed by **two distinct subsystems**:

1. **Agent tool invocation** — The LLM calls `build_software` directly during a conversation (`BuildSoftwareTool`, `core.rs:907-1016`)
2. **Reflex compiler** — Background daemon that detects repeated patterns and auto-compiles WASM tools (`src/agent/reflex.rs:680-696`)
3. **Self-repair** — Repairs broken WASM tools by re-building them (`src/agent/self_repair.rs:266-301`)

---

## 2. Key Types and Data Structures

### Core Types (`src/tools/builder/core.rs`)

| Type | Lines | Purpose |
|---|---|---|
| `BuildRequirement` | 50-68 | Input spec: name, description, software_type, language, I/O specs, dependencies, capabilities |
| `SoftwareType` enum | 73-84 | `WasmTool`, `CliBinary`, `Library`, `Script`, `WebService` |
| `Language` enum | 89-96 | `Rust`, `Python`, `TypeScript`, `JavaScript`, `Go`, `Bash` |
| `BuildResult` | 137-168 | Output: build_id, artifact_path, logs, success/error, iterations, test results, validation warnings |
| `BuildLog` | 171-177 | Timestamped log entry with phase and message |
| `BuildPhase` enum | 180-194 | `Analyzing`→`Scaffolding`→`Implementing`→`Building`→`Testing`→`Fixing`→`Validating`→`Registering`→`Packaging`→`Complete`/`Failed` |
| `BuilderConfig` | 197-215 | Runtime config: build_dir, max_iterations (default 10), timeout (600s), validate_wasm, run_tests, auto_register, wasm_output_dir |
| `SoftwareBuilder` trait | 233-247 | Async trait: `analyze()`, `build()`, `repair()` |
| `LlmSoftwareBuilder` | 250-254 | LLM-powered implementation: holds config, `Arc<dyn LlmProvider>`, `Arc<ToolRegistry>` |
| `BuildSoftwareTool` | 907-1016 | `Tool` trait impl wrapping `SoftwareBuilder` for agent use |

### Configuration Types

| Type | File | Lines | Purpose |
|---|---|---|---|
| `BuilderSettings` | `src/settings.rs` | 900-942 | User-facing settings persisted to disk: enabled, build_dir, max_iterations (20), timeout_secs (600), auto_register |
| `BuilderModeConfig` | `src/config/builder.rs` | 8-61 | Resolved config with env-var overrides: `BUILDER_ENABLED`, `BUILDER_DIR`, `BUILDER_MAX_ITERATIONS`, `BUILDER_TIMEOUT_SECS`, `BUILDER_AUTO_REGISTER` |

### Supporting Module Types

| Module | Types | Purpose |
|---|---|---|
| `templates.rs` | `Template`, `TemplateEngine`, `TemplateType` | Pre-built scaffolds for WASM HTTP tools, transform tools, compute tools, CLI binaries, Python/Bash scripts |
| `testing.rs` | `TestCase`, `TestSuite`, `TestResult`, `TestHarness`, `ExpectedField` | Automated testing of built WASM tools before registration |
| `validation.rs` | `WasmValidator`, `ValidationResult`, `ValidationError`, `ExportInfo`, `ImportInfo` | Validates WASM modules: required exports, allowed imports, size limits |

---

## 3. Data Flow — How a Build Request Flows

### Agent-Initiated Build (LLM calls `build_software`)

```
User message → Agent loop → LLM decides to call `build_software` tool
    │
    ▼
BuildSoftwareTool::execute()                     [core.rs:953-1011]
    │
    ├─ 1. builder.analyze(description)           [core.rs:813-861]
    │      → LLM generates JSON → parsed into BuildRequirement
    │      → type/language overrides from tool params
    │
    ├─ 2. builder.build(&requirement)            [core.rs:863-887]
    │      ├─ Create project dir: {build_dir}/{name}
    │      ├─ Clean existing dir
    │      └─ execute_build_loop() with timeout   [core.rs:501-766]
    │           │
    │           ├─ Build system prompt            [core.rs:283-326]
    │           │   (includes WASM context if software_type == WasmTool)
    │           ├─ Get tool definitions          [core.rs:268-280]
    │           │   (shell, read_file, write_file, list_dir, apply_patch, http)
    │           ├─ Loop until complete or max_iterations:
    │           │   ├─ reasoning.respond_with_tools() → LLM response
    │           │   ├─ Text response → check for completion / force tool use
    │           │   └─ ToolCalls → execute each via ToolRegistry
    │           │       └─ ToolResult fed back to LLM for next iteration
    │           └─ Returns BuildResult (success/failure with logs)
    │
    └─ 3. Return JSON summary to LLM
         {build_id, name, success, artifact_path, iterations, phases}
```

### Reflex-Initiated Build (Background)

```
Reflex compiler detects repeated pattern      [src/agent/reflex.rs:680-696]
    │
    ├─ Create BuildRequirement (wasm_tool, rust)
    ├─ builder.build(&req)  ← same LlmSoftwareBuilder
    └─ On success: upsert_reflex_pattern() in DB
```

### Self-Repair Build

```
DefaultSelfRepair detects broken tool       [src/agent/self_repair.rs:266-301]
    │
    ├─ Create BuildRequirement from BrokenTool info
    ├─ builder.build(&req)  ← same LlmSoftwareBuilder
    └─ On success: auto-register repaired tool
```

### Initialization Flow

```
main.rs → AppBuilder::build_all()             [src/app.rs:897-900]
    │
    └─ init_tools()                            [src/app.rs:473-484]
        │
        ├─ if config.builder.enabled AND (allow_local_tools OR !sandbox.enabled):
        │   └─ tools.register_builder_tool(llm, Some(builder_config))  [registry.rs:755-775]
        │       ├─ register_dev_tools()  (shell, read/write_file, list_dir, apply_patch)
        │       ├─ LlmSoftwareBuilder::new(config, llm, tools)
        │       └─ register(BuildSoftwareTool::new(builder))
        │
    └─ Store Arc<dyn SoftwareBuilder> in AppComponents.builder

main.rs:976 → AgentDeps { builder: components.builder, ... }
    │
    ├─ agent_loop.rs:501-502 → self_repair.with_builder(builder, tools)
    └─ agent_loop.rs:741     → reflex_compiler uses builder
```

---

## 4. Integration Points with v2 Engine (Bridge, Engine V2)

### Bridge Effect Adapter

The builder's `build_software` tool is **recognized** by the v2 bridge effect adapter as a "side-effect" tool that requires special handling:

- **File:** `src/bridge/effect_adapter.rs:1018-1028`
- `build_software` / `build-software` is listed alongside `create_job`, `cancel_job`, `routine_create`, `routine_list`, `routine_fire` as tools that the bridge treats as requiring effect-batching (not passthrough).

This is the **only** integration point with the v2 engine/bridge. The builder itself does **not** use the bridge or engine V2 for its own execution — it uses the legacy `Reasoning`/`LlmProvider` API directly.

### What's Missing

- **No bridge-mediated execution**: The builder calls `reasoning.respond_with_tools()` directly (`core.rs:580-583`), not through the v2 engine bridge.
- **No engine V2 tool definitions**: The builder's `get_build_tools()` (`core.rs:268-280`) fetches tool definitions from `ToolRegistry::tool_definitions_for()`, not from the v2 engine's tool registry.
- **No v2 approval/gate integration**: `BuildSoftwareTool::requires_approval()` returns `UnlessAutoApproved` (`core.rs:1013-1015`), bypassing the v2 gate system.

---

## 5. GAPS: Outdated, Broken, or Diverged from v2 Patterns

### 5.1 — Templates and Validation Never Used in Build Loop

**Severity: High (Dead Code)**

The `templates.rs`, `testing.rs`, and `validation.rs` modules are fully implemented and exported (`mod.rs:26-37`), but **never invoked** during the build loop:

- `TemplateEngine` — Not referenced in `execute_build_loop()`. The build loop relies entirely on LLM-generated code with no scaffolding templates.
- `WasmValidator` — Not called in `execute_build_loop()`. The `BuilderConfig.validate_wasm` flag (`core.rs:208`) is set but never checked.
- `TestHarness` — Not called. The `BuilderConfig.run_tests` flag (`core.rs:210`) is set but never checked. `tests_passed`/`tests_failed` in `BuildResult` are always 0.

**Evidence:** Search of `core.rs` for `TemplateEngine|TestHarness|WasmValidator` returns only `validation_warnings` field references (always `Vec::new()`).

### 5.2 — No WASM Auto-Registration

**Severity: High (Dead Feature)**

`BuilderConfig.auto_register` (`core.rs:212`) defaults to `true`, and `BuildResult.registered` (`core.rs:167`) exists, but:

- The build loop **never registers** built WASM tools with the `ToolRegistry`.
- `BuildResult.registered` is always `false` in all code paths (lines 573, 624, 673).
- `wasm_output_dir` (`core.rs:214`) is `None` by default and never set.
- `self_repair.rs:336` has a log message for "auto-registered by builder" but the builder itself doesn't do registration.

### 5.3 — Stale `edition = "2021"` in WASM Templates

**Severity: Medium**

The WASM tool context template (`core.rs:380`) generates `Cargo.toml` with `edition = "2021"`, but the repo-wide coding rules (AGENTS.md) require **Rust 2024 edition**. The template templates in `templates.rs:209` also use `edition = "2021"`.

This means the builder produces software that doesn't conform to the project's own edition standard.

### 5.4 — Legacy v1 LLM API (Reasoning/LlmProvider)

**Severity: Medium (v2 Divergence)**

The builder uses the **legacy v1 LLM interaction layer**:

- `Reasoning::new(self.llm.clone())` (`core.rs:513`) — v1 reasoning engine
- `reasoning.respond_with_tools(&reason_ctx)` (`core.rs:581`) — v1 tool-calling
- `reasoning.respond(&ctx)` (`core.rs:849`) — v1 text response
- `ChatMessage::system()`, `ChatMessage::user()`, `ChatMessage::assistant_with_tool_calls()` — v1 message types

The v2 engine/bridge layer is not used. Other systems (agent loop, orchestrator) have migrated to v2 patterns. The builder is a v1 holdover.

### 5.5 — No Database Persistence for Build Records

**Severity: Medium**

The `Database` trait (`src/db/mod.rs`) has **zero** builder-related methods. No `builder` search matches in `db/mod.rs` or `db/postgres.rs`. Build records, logs, and results are ephemeral — they exist only in memory and in the `BuildResult` returned to the LLM.

This means:
- No build history persistence
- No way to query past builds
- No audit trail for what was built
- No way to re-trigger or repair a past build from DB

### 5.6 — No TODO/FIXME Comments But Silent Gaps

**Severity: Low (Observation)**

There are **zero** `TODO`, `FIXME`, `HACK`, `XXX`, `todo!()`, `unimplemented!()`, or `panic!()` in the builder module. This is unusual — the gaps are silent omissions, not marked work-in-progress.

### 5.7 — Reflex Compiler Dependency

**Severity: Medium (Coupling)**

The builder is a **hard dependency** of the Reflex compiler (`src/agent/reflex.rs:696`):

```rust
match self.builder.build(&req).await {  // reflex.rs:696
```

If the builder is disabled or misconfigured, the reflex compiler silently warns but cannot compile patterns. The coupling is tight — `reflex.rs` imports `BuildRequirement`, `Language`, `SoftwareType`, `SoftwareBuilder` directly from the builder module (`reflex.rs:24`).

### 5.8 — `cleanup_on_failure` Config Divergence

**Severity: Low (Config Bug)**

- `BuilderConfig::default()` sets `cleanup_on_failure: false` (`core.rs:223`)
- `BuilderModeConfig::to_builder_config()` sets `cleanup_on_failure: true` (`config/builder.rs:55`)

This means the default in-memory config keeps failed build dirs for debugging, but the production config (from settings) cleans them up immediately. The `cleanup_on_failure` flag is **never read** anywhere in the code anyway — it's dead config.

### 5.9 — `max_iterations` Default Mismatch

**Severity: Low (Config Bug)**

- `BuilderConfig::default()`: `max_iterations = 10` (`core.rs:221`)
- `BuilderSettings::default()`: `max_iterations = 20` (`settings.rs:924`)
- `BuilderModeConfig::default()`: `max_iterations = 20` (`config/builder.rs:28`)

The in-code default is 10, but user-facing settings default to 20. In practice the settings path is always used in production, but if `BuilderConfig::default()` is used directly (e.g., in `register_builder_tool` with `None` config), the iteration limit is halved.

### 5.10 — WASM Template Host Function ABI Mismatch

**Severity: High (Broken Templates)**

The template files in `templates.rs` use a **raw FFI ABI** for host functions:

```rust
#[link(wasm_import_module = "env")]
extern "C" {
    fn host_log(level: i32, ptr: *const u8, len: usize);
    fn host_http_request(...) -> i32;
}
```

But the actual WASM tool system uses **WIT bindings** via `wit_bindgen` (`core.rs:395-401`):

```rust
wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "wit/tool.wit",
});
```

The templates generate code that won't compile with the current WASM tool runtime. The `WasmToolRuntime` uses the WIT/component model, not raw `extern "C"` imports. The templates are fundamentally incompatible with the actual WASM tool loading infrastructure.

### 5.11 — `find_artifact` Path Detection Fragile

**Severity: Medium**

`find_artifact()` (`core.rs:787-808`) uses hardcoded path patterns to find build artifacts:

```rust
(SoftwareType::WasmTool, Language::Rust) => {
    crate::tools::wasm::wasm_artifact_path(project_dir, &requirement.name.replace('-', "_"))
}
```

It doesn't verify the file exists, doesn't handle `cargo-component` output naming conventions, and has no fallback for non-standard output locations.

### 5.12 — No Sandbox/Container Integration

**Severity: Medium**

The builder executes tools via `self.tools.get(tool_name).await` and calls `tool.execute()` with a `JobContext::default()` (`core.rs:776-783`). This runs **in the main agent process** — no Docker sandbox, no container isolation.

The activation condition in `app.rs:474-475` even reflects this:
```rust
if self.config.builder.enabled
    && (self.config.agent.allow_local_tools || !self.config.sandbox.enabled)
```

The builder is only enabled when local tools are allowed OR sandbox is disabled — it cannot run in a containerized environment.

### 5.13 — Completion Detection is Heuristic String Matching

**Severity: Medium**

Build completion is detected by string-matching the LLM's text response (`core.rs:644-648`):

```rust
if response_lower.contains("build complete")
    || response_lower.contains("successfully built")
    || response_lower.contains("all tests pass")
    || response_lower.contains("complete")
```

The `"complete"` substring match is especially fragile — any LLM response mentioning "complete" in any context will end the build loop prematurely.

### 5.14 — `web_service` Type Declared But Not Supported

**Severity: Low**

`SoftwareType::WebService` is defined (`core.rs:83`) but:
- Not in the `BuildSoftwareTool` schema enum (`core.rs:940-941` only lists `wasm_tool`, `cli_binary`, `library`, `script`)
- No build commands defined for it
- No artifact path handling

### 5.15 — Language Enum Serde Naming Mismatch

**Severity: Low (Latent Bug)**

The `Language` enum uses `#[serde(rename_all = "snake_case")]` (`core.rs:88`), which produces `"type_script"` and `"java_script"` for `TypeScript` and `JavaScript` respectively (confirmed in tests at `core.rs:1148-1149`). The LLM is instructed to return `"typescript"` and `"javascript"` in the analyze prompt (`core.rs:835`), which would fail to deserialize.

---

## 6. Key File Paths with Line Numbers

### Builder Core
| Section | File:Line |
|---|---|
| Module docs & architecture diagram | `src/tools/builder/core.rs:1-31` |
| `BuildRequirement` struct | `src/tools/builder/core.rs:50-68` |
| `SoftwareType` enum | `src/tools/builder/core.rs:73-84` |
| `Language` enum + methods | `src/tools/builder/core.rs:89-133` |
| `BuildResult` struct | `src/tools/builder/core.rs:137-168` |
| `BuildPhase` enum | `src/tools/builder/core.rs:180-194` |
| `BuilderConfig` struct + defaults | `src/tools/builder/core.rs:197-230` |
| `SoftwareBuilder` trait | `src/tools/builder/core.rs:233-247` |
| `LlmSoftwareBuilder` struct | `src/tools/builder/core.rs:250-265` |
| `get_build_tools()` | `src/tools/builder/core.rs:268-280` |
| `build_system_prompt()` | `src/tools/builder/core.rs:283-326` |
| `wasm_tool_context()` (template) | `src/tools/builder/core.rs:329-498` |
| `execute_build_loop()` — main loop | `src/tools/builder/core.rs:501-766` |
| `execute_build_tool()` — tool dispatch | `src/tools/builder/core.rs:769-784` |
| `find_artifact()` | `src/tools/builder/core.rs:787-808` |
| `analyze()` — LLM requirement parsing | `src/tools/builder/core.rs:813-861` |
| `build()` — orchestrates build with timeout | `src/tools/builder/core.rs:863-887` |
| `repair()` — re-builds failed build | `src/tools/builder/core.rs:889-903` |
| `BuildSoftwareTool` impl | `src/tools/builder/core.rs:907-1016` |
| Unit tests | `src/tools/builder/core.rs:1018-1400` |

### Module Structure
| File | Purpose |
|---|---|
| `src/tools/builder/mod.rs:26-37` | Module declarations and re-exports |
| `src/tools/builder/templates.rs` | Template engine (6 template types, **unused in build loop**) |
| `src/tools/builder/testing.rs` | Test harness for WASM tools (**unused in build loop**) |
| `src/tools/builder/validation.rs` | WASM module validator (**unused in build loop**) |

### Configuration
| Section | File:Line |
|---|---|
| `BuilderSettings` (user settings) | `src/settings.rs:900-942` |
| `BuilderModeConfig` (resolved config) | `src/config/builder.rs:8-61` |
| `to_builder_config()` conversion | `src/config/builder.rs:49-60` |
| Config tests | `src/config/builder.rs:63-94` |

### Registration & Initialization
| Section | File:Line |
|---|---|
| Module re-exports from `tools/mod.rs` | `src/tools/mod.rs:29-33` |
| `PROTECTED_TOOL_NAMES` includes `build_software` | `src/tools/registry.rs:59` |
| `register_dev_tools()` | `src/tools/registry.rs:429-437` |
| `register_builder_tool()` | `src/tools/registry.rs:755-775` |
| App init: builder tool registration | `src/app.rs:473-484` |
| App init: dev tools dedup logic | `src/app.rs:878-884` |
| App init: `AppComponents.builder` field | `src/app.rs:60` |
| Agent deps: builder wired into `AgentDeps` | `src/main.rs:976` |
| Agent loop: self-repair setup with builder | `src/agent/agent_loop.rs:501-502` |
| Agent loop: reflex compiler uses builder | `src/agent/agent_loop.rs:741-753` |

### Agent Consumers
| Section | File:Line |
|---|---|
| Self-repair: builder field + with_builder() | `src/agent/self_repair.rs:90-106` |
| Self-repair: repair_broken_tool() | `src/agent/self_repair.rs:266-301` |
| Reflex: imports from builder module | `src/agent/reflex.rs:24` |
| Reflex: compiler build call | `src/agent/reflex.rs:680-696` |

### v2 Bridge Integration
| Section | File:Line |
|---|---|
| Effect adapter recognizes `build_software` | `src/bridge/effect_adapter.rs:1018-1028` |

### Permissions
| Section | File:Line |
|---|---|
| `build_software` seeded as `AskEachTime` | `src/tools/permissions.rs:76` |

---

## 7. Summary Assessment

### Current State: **Partially Functional, Largely Incomplete**

The Software Builder has a well-designed architecture with clear types, a multi-phase build loop, and three consumer subsystems (agent tool, reflex compiler, self-repair). However, it is in a **v1-era state** with significant gaps:

### What Works
- ✅ `build_software` tool is registered and callable by the LLM
- ✅ The LLM-driven build loop executes (analyze → scaffold → implement → build → fix)
- ✅ Reflex compiler can trigger builds
- ✅ Self-repair can trigger builds
- ✅ Configuration resolution from settings + env vars works correctly
- ✅ Protected tool name prevents shadowing
- ✅ Permission gating (`AskEachTime` seeded)

### What Doesn't Work / Is Dead
- ❌ Templates never used in build loop (dead code)
- ❌ Validation never run (`validate_wasm` flag ignored)
- ❌ Testing never run (`run_tests` flag ignored, tests_passed/failed always 0)
- ❌ Auto-registration of built WASM tools never happens
- ❌ `wasm_output_dir` never set or used
- ❌ `cleanup_on_failure` never read
- ❌ No build history persisted to database
- ❌ Templates use incompatible FFI ABI (won't compile with current WASM runtime)

### What's Diverged from v2
- ⚠️ Uses legacy `Reasoning`/`LlmProvider` API instead of v2 engine/bridge
- ⚠️ No v2 approval/gate integration
- ⚠️ No container/sandbox integration (runs in main process only)
- ⚠️ Templates use `edition = "2021"` (repo requires 2024)

### Recommendations for Christopher

1. **Immediate**: Fix the `Language` serde naming bug (`type_script`/`java_script` vs `typescript`/`javascript`) — this silently breaks LLM-generated `BuildRequirement` parsing.
2. **Short-term**: Wire up the `WasmValidator` and `TestHarness` into `execute_build_loop()` — the code exists, it just needs to be called.
3. **Short-term**: Implement WASM auto-registration (copy to `wasm_output_dir`, register with `ToolRegistry`).
4. **Medium-term**: Migrate from v1 `Reasoning` API to v2 engine/bridge — aligns with the codebase migration.
5. **Medium-term**: Add build record persistence to the `Database` trait.
6. **Medium-term**: Fix templates to use WIT bindings instead of raw `extern "C"` FFI, and update `edition` to 2024.
7. **Long-term**: Consider containerizing builds (run `cargo-component` inside Docker sandbox instead of main process).
