# External Worker Security Audit (2026-06-23)

> **Current status (updated 2026-08-04): IMPLEMENTED / HISTORICAL RECORD.**
> Task context population, credential cleanup, connection-failure load-balancer
> failover, graceful pool draining, and integration coverage have landed. Protocol
> errors and task timeouts still do not fail over to another endpoint. Bounded,
> opt-in conversation history plumbing also landed after this audit. Codex was
> removed and Nanocode was later retired; their sections below are historical.
> The shared orchestrator fixes apply to supported Pebble, OpenCode, and Kimi
> Code workers.

> **Audit baseline: 2026-07-09 (v1.1.9).** This document retains the worker set
> and findings as reviewed at that time.

## Context
External worker enhancement implementation revealed four problems:
1. **Pool release gap** – connections never returned to pool after task completion
2. **Empty TaskContext** – conversation_history and metadata empty in worker requests  
3. **Credential env leakage** – environment variables not cleaned up after tasks
4. **LB failover** – LoadBalancer lacks health checks/circuit breakers

This audit focuses on problems #2 and #3, verifying fixes and identifying remaining risks.

## Problem #2 – Empty TaskContext Fix

### Root Cause
`CreateJobTool::execute_external` called `build_task_context()` with:
- `conversation_history: vec![]`
- `metadata: HashMap::new()`

Despite `JobContext` having:
- `conversation_id: Option<Uuid>` (could fetch actual history)
- `metadata: serde_json::Value` (populated by agent loop)

### Current Fix (supersedes the audit-era `conv_*` bridge)

`JobContext` now carries a typed transient `prompt_context`. Legacy chat derives
it from already-scoped `initial_messages`; Engine V2 forwards the thread's typed
context through `ThreadExecutionContext`. `create_job` defaults
`context_scope` to `none`; `recent` selects only user/assistant messages, newest
first within limits, then restores chronological order. Defaults are 12 messages,
2,000 characters per message, and 12,000 total characters, clamped to hard
ceilings of 50 / 10,000 / 50,000.

`CreateJobTool::execute_external` sends the selected messages in
`TaskContext.conversation_history` and deliberately sends an empty
`TaskContext.metadata` map. Worker prompt builders for Pebble, OpenCode, and
Kimi Code consume only that bounded history as JSON-labeled untrusted reference
data; environment and operational metadata stay outside model prompts. See
`EXTERNAL-WORKER-PROMPT-CONTEXT.md` for the current contract and tests.

### Remaining Limitations

- Context is opt-in and scoped to the messages already supplied to the active
  legacy chat or Engine V2 thread; there is no global transcript lookup.
- Unknown/system/tool roles are filtered before `JobContext.prompt_context`.
- `TaskContext.metadata` remains wire-level transport metadata and is empty on
  this path rather than a prompt-context channel.

## Problem #3 – Credential Environment Cleanup Audit

### Worker Container Analysis

#### 1. Nanocode Worker (`lunarcode4lunarwing/scripts/nanocode_task_executor.ts`)
**Cleanup**: ✅ **Excellent**
```typescript
} finally {
    clearTimeout(timeout);
    // Restore original environment to prevent credential leakage between tasks
    for (const key of injectedKeys) {
        if (savedValues[key] === undefined) {
            delete process.env[key];
        } else {
            process.env[key] = savedValues[key];
        }
    }
}
```
- Saves original values before injection
- Deletes injected keys or restores originals in `finally` block
- **Risk**: Low

#### 2. Codex Worker (`codex4lunarwing/scripts/codex_task_executor.ts`)

> **Note**: The Codex worker was removed in v1.1.9. This section is retained for historical reference.

**Cleanup**: ✅ **Adequate**
```typescript
proc = spawn(["codex", ...args], {
    cwd: workDir,
    env: { ...process.env, ...extraEnv },
    // ...
});
```
- Environment passed to subprocess only
- Subprocess killed on timeout/abort via `kill("SIGTERM")` → `kill("SIGKILL")`
- No main process leakage (never writes to `process.env`)
- **Risk**: Low-medium – relies on subprocess termination for cleanup

#### 3. Pebble Worker (`pebble4lunarwing/src/executor.rs`)
**Cleanup**: ✅ **Adequate**
```rust
cmd.kill_on_drop(true);

for (key, value) in &request.context.environment {
    cmd.env(key, value);
}
```
- `kill_on_drop(true)` ensures subprocess termination
- Environment scoped to subprocess via `Command::env()`
- **Risk**: Low-medium – relies on subprocess drop

#### 4. Orchestrator (`ic/src/orchestrator/external_worker.rs`)
**Logging**: ✅ **Secure**
- No tracing logs of `context.environment` content
- Serialization to WebSocket is intentional for worker communication
- No credential leakage in logs

### Risk Assessment Matrix

| Worker | Cleanup Mechanism | Risk Level | Notes |
|--------|-------------------|------------|-------|
| Nanocode | Explicit delete/restore in finally block | Low | Best practice |
| Codex | Subprocess termination | Low-medium | Acceptable for single-use subprocess *(worker removed v1.1.9)* |
| Pebble | Subprocess drop (kill_on_drop) | Low-medium | Acceptable for short-lived tasks |
| Orchestrator | No logging | Low | Secure |

### Potential Improvements

1. **Codex worker** *(removed in v1.1.9 — N/A)*: Add explicit cleanup for belt-and-suspenders:
   ```typescript
   // After proc.killed check
   for (const key of Object.keys(extraEnv)) {
       delete process.env[key];  // Though never set, defensive
   }
   ```

2. **Test coverage**: Add integration tests for credential cleanup scenarios
3. **LoadBalancer health checks**: Remaining problem #4 – needs circuit breaker implementation

## Residual Risks

### 1. Memory Exposure
- Credentials serialized to JSON over WebSocket (necessary)
- Could be captured by network sniffing between LunarWing and worker container
- **Mitigation**: Use `wss://` (TLS) in production deployments

### 2. Subprocess Zombies
- If subprocess crashes before cleanup, credentials remain in memory until OS reclaims
- **Mitigation**: Short task timeouts (default 300s) limit exposure window

### 3. Shared Worker Containers
- Multiple tenants sharing same worker container (multi-tenant deployments)
- **Mitigation**: Each tenant gets own container per `lunarwing-mt-admin.sh` isolation

## Recommendations

### Immediate (Release 1.1.6)
1. ✅ Implement pool release fix (problem #1)
2. ✅ Implement TaskContext population (problem #2)
3. ✅ Verify credential cleanup (problem #3 audit complete)
4. Consider minimal LB failover (problem #4) – retry to next endpoint on connection error

### Future
1. **Endpoint health:** proactive health monitoring remains optional future work.
2. **Retry classification:** protocol errors and task timeouts currently do not
   fail over; define idempotency and retry safety before broadening the predicate.
3. **Defensive cleanup:** ~~Add explicit env var deletion to Codex worker~~ *(N/A — Codex worker removed in v1.1.9)*.
4. **Integration tests:** add chaos tests for credential cleanup scenarios.

## Conclusion

Problems #2 and #3 are addressed:
- **#2:** bounded, opt-in, typed conversation history now reaches supported
  worker prompts without operational metadata.
- **#3:** credential cleanup mechanisms are functionally adequate with low risk.

Connection-failure load-balancer failover has landed; proactive health and safe
protocol/timeout failover remain open.
