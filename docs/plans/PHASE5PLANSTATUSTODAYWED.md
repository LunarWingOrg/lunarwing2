Full Branch Plan Status
This branch was meant to execute the nine-task Engine V2 Local Compatibility Completion plan. It is roughly Tasks 1–3 complete, Task 4 partial, Tasks 5–9 unstarted.
Completed and committed
Task 1 — Engine V2 environment support
Status: Complete in e4b1c87
•
Shared EngineV2EnvGuard
•
Environment restoration/reset coverage
•
Existing channel-delivery target migrated to shared guard
Task 2 — Phase 5 fidelity gaps
Status: Complete in e4b1c87
•
WASM StreamChunk no-op regression
•
Original incoming metadata regression
•
DarkIRC scope-isolation regressions
•
WeeChat DM/group scope-isolation regressions
Implemented but uncommitted
Task 3 — Real MCP compatibility
Status: Functionally complete; final review/check incomplete
Implemented:
•
Real MCP initialize/list/call through Engine V2
•
Real OAuth discovery and dynamic client registration
•
Real token exchange and encrypted storage
•
Same-thread OAuth callback resume
•
Exactly-once MCP execution
•
Credential-leak assertions
•
Provider-safe tool-name resolution
•
Case-insensitive authentication detection
•
Gate resume-output preservation
•
Dynamic lease refresh after activation
•
OAuth callback Engine V2 resume
•
Focused test modules under ic/tests/mcp_compat/
Passing evidence:
•
MCP integration: 3/3
•
MCP unit suite: 191/191
•
Effect adapter: 25/25
•
Extension tools: 14/14
•
Tool registry: 11/11
•
Web callback tests: 19/19
•
Rustfmt check passed
Incomplete:
•
Cargo check was stopped at 971/973; no result.
•
No LSP/security/final architecture review.
•
No commit.
•
oauth_callback_for_test and callback fallback resolution need review before merging.
Partially implemented
Task 4 — Real WASM tool compatibility
Status: Partial, uncommitted
Present:
•
ic/tests/engine_v2_wasm_tool.rs
•
ic/tests/fixtures/test-echo-tool/
•
WASI P1/P2 compiled artifacts
•
Cargo target registration
Historical partial evidence:
•
The worker reported three integration tests passing before timing out.
Still needed:
•
Re-run target from current tree.
•
Run adjacent WASM host tests.
•
Prove production WIT files are unchanged.
•
Review fixture source, generated bindings, ignored build artifacts, capability-denial proof, and call-ID assertions.
•
Commit only intended source/fixture files—not target/.
Not started
Task 5 — Skill compatibility
Missing:
•
engine_v2_skill_selection.rs
•
with_seeded_skill()
•
Discovery/migration/idempotence/selection/context tests
Task 6 — TensorZero-shaped streaming
Missing:
•
engine_v2_tensorzero_streaming.rs
•
Loopback SSE fixture
•
Text/tool fragmentation tests
•
Mid-stream error and premature EOF tests
Task 7 — Browser Engine V2 E2E
Missing:
•
Explicit release-binary fixture
•
Isolated Engine V2 server/page fixtures
•
Mock streaming/approval/auth/interrupt scripts
•
Browser scenario
•
Release build
Task 8 — Documentation
No compatibility evidence has been recorded in:
•
docs/architecture/ENGINE-V2.md
•
docs/proposals/ENGINE_LLM_STREAMING.md
•
ic/FEATURE_PARITY.md
•
Phase 5 plan
Task 9 — Complete local gate
Not run:
•
Default/PostgreSQL/libSQL/all-feature checks
•
Full engine, bridge, gate, WASM, and channel suites
•
All focused integration targets
•
Browser E2E
•
Clippy
•
ABI/WIT/source invariants
•
Repository safety/boundary scripts
•
Release artifact handoff
Clippy is currently blocked because cargo-clippy is not installed.
Current worktree concerns
•
No commits after e4b1c87.
•
Task 3 and Task 4 are mixed in one dirty worktree.
•
There is unrelated rustfmt-only churn in:
•
skill_tracker.rs
•
mission.rs
•
parts of 
router.rs
•
parts of 
wrapper.rs
•
Separate/revert that incidental churn before committing.
•
No live tenant was modified.
•
No build, Cargo, or rustc processes remain.
Recommended continuation order
1.
Review and commit Task 3 separately.
2.
Clean and fully verify Task 4.
3.
Implement Tasks 5 and 6.
4.
Implement browser Task 7.
5.
Update documentation.
6.
Run Task 9’s complete local gate.
7.
Only then create the disposable-tenant rollout plan.
