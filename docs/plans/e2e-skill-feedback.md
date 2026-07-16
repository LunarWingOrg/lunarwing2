---
plan name: e2e-skill-feedback
plan description: Live-tenant E2E of Engine V2 skill feedback
plan status: active
---

## Idea
Execute a live end-to-end test of the uncommitted Engine V2 skill usage feedback mechanism on a disposable OpenRC tenant. Provision a new tenant, inject the current worktree's binary (with the uncommitted changes), enable SKILL_SELF_IMPROVEMENT, seed an extracted V2 skill, submit a real Engine V2 thread, verify the skill's usage metric increments exactly once, then destroy the tenant.

## Implementation
- Pre-flight: inspect host state (OpenRC, ports registry, existing tenants, DB connectivity, shared-owner id)
- Build lunarwing release binary from current worktree (contains uncommitted skill-feedback changes)
- Select disposable tenant name + allocate next-free port block from /etc/lunarwing/ports.json
- Provision via add-tenant in tmux (taskset -c 0-5, -j6); clone repo, create user, alloc ports, write env, start PG, render OpenRC units
- Inject prebuilt binary into tenant copy + patch-env + add SKILL_SELF_IMPROVEMENT=true to lunarwing.env
- Seed extracted V2 skill (source=Extracted, keyword-matching test goal) into tenant's memory_documents table under shared-owner scope
- Start tenant via start-tenant; poll /healthz and /agent/status until ready
- Submit Engine V2 thread via POST /api/chat/send (Bearer token, body {content}) with goal matching skill keyword
- Verify metric delta in DB: usage_count and success_count incremented exactly once; metadata key cleared
- Cleanup: stop-tenant + remove-tenant --purge; validate ports deallocated, no leftover OpenRC units
- Post-cleanup validation: confirm no residual state, report results

## Required Specs
<!-- SPECS_START -->
<!-- SPECS_END -->