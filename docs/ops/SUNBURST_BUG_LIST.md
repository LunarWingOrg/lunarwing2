Open Bugs — Cumulative Definitive List

Sources: Sunburst instance (cmc tenant, 2.0.2.0–2.0.3), 7 testing tenants, cmc's bug tracker, Dwarfstar testing tenant
🔴 HIGH — Core Engine Bugs
Bug 	Name 	Status
BUG-001 	Skill File Duplication on Restart (Gap #2) 	⚠️ Dormant (SIS disabled), NOT fixed. Returns if SIS re-enabled. Reached 132 files for 9 skills on 2.0.3. No content-hash dedup before insert.
BUG-002 	Thread JSON Indexing Leak (Gap #1) 	🔴 Open — memory_search() returns duplicated chunks from engine/.runtime/threads/active/*.json. Needs dedicated reproduction test.
BUG-003 	Mission Thread Goal Pollution 	🔴 Open — trigger_payload.thread_goals includes goals from OTHER mission-triggered threads. Same Gap #2 family.
BUG-014 	Trigger Payloads Not Propagating to Mission Thread State 	🔴 Open — breaks event-driven self-improvement missions. (From 7 testing tenants)
BUG-015 	Lease Cleanup Leak 	🔴 Open — 582 active leases with 0 active threads. Leases not cleaned up after thread completion. (From 7 testing tenants)
BUG-018 	v2 Orchestrator Gate Collision (cmc BUG-001) 	✅ RESOLVED — fixed in 2.0.3.0, four-layer fix.
BUG-020 	Context Explosion from Uncapped ActionResult (cmc BUG-003) 	✅ FIXED — Sunburst patch, 8K cap + non-retryable overflow.
BUG-021 	ssh_git Null Ref Parameter Handling (Dwarfstar BUG-001) 	🔴 Open — null serialized as literal "null" to git. Workaround: always pass explicit strings.
BUG-022 	Job Stuck Pending Forever — Ghost Job (Dwarfstar BUG-002) 	🔴 Open — job shows pending in list_jobs but job_status returns "Job not found". DB row exists but invisible to status endpoint.
BUG-023 	External Worker Job Marked FAILED Despite Success (Dwarfstar BUG-003) 	🔴 Open — worker succeeds (clone, commit, push) but host marks FAILED on timeout. started_at: null despite job actually running.
🟡 MEDIUM — Integration/Channel Bugs
Bug 	Name 	Status
BUG-004 	Multica Registration — PathNotAllowed 	🟡 Open — allowlist permits /api/ but registration uses /cotter-openclaw/api/daemon/register. Repeating every 30s in logs.
BUG-005 	Multica Daemon Connection Refused 	🟡 Open — 127.0.0.1:6680/poll refused. Local daemon not running or wrong port. May be related to BUG-004.
BUG-006 	DarkIRC Adapter Not Deployed as Service 	🟡 Open — darkirc_adapter.py exists but no init.d/OpenRC service entry. Port 6680 refused.
BUG-007 	WeeChat Relay 401 Unauthorized 	🟡 Open — /api/wait returns HTTP 401. Suspected relay password mismatch after Jul 31 config change.
BUG-016 	WeeChat Buffer Restore (OpenRC Root-vs-Tenant) 	🟡 Known, deferred — OpenRC runs as root, WeeChat expects tenant-user context. (From 7 testing tenants)
BUG-019 	WeeChat Relay Env Binding (cmc BUG-002) 	❓ Unknown — flagged but never confirmed fixed or still broken. Possibly related to BUG-007.
🟢 LOW — Configuration/Minor Issues
Bug 	Name 	Status
BUG-008 	Tool Auth Flags — web-search, llm-context, llm-context-tool 	🟢 Open — all show needs_setup=true. API keys not wired in. Pre-existing.
BUG-009 	Routine Tools Not Callable in Engine v2 	🟢 Open — routine_list/create/update/delete raise RuntimeError: not available in engine v2. Only routine_history works. By design or bug?
BUG-010 	Tool Parameter Discovery Gap ("Bug B") 	🟢 Open — tool_info doesn't expose all parameters for some tools.
BUG-011 	Podman Containers Stuck in "starting" Health State 	🟢 Open — lunarwing-vision-sunburst and lunarwing-opencode-sunburst show "(starting)" after 7+ hours.
BUG-012 	feishu/telegram Channel Auto-Activation Failures 	🟢 Open — stale configs reference uninstalled channels.
BUG-013 	Secret Leak Warnings in Logs 	🟢 Open — container IDs and config values logged at warning level. Noisy but no functional impact.
BUG-017 	Podman Hard-Kill Fails 	🟢 Open — mkdir /run/user/0/libpod: permission denied. Can't force-stop containers without root. (From 7 testing tenants)
BUG-024 	LUNARWING_BASE_DIR Not Inherited via sudo (Dwarfstar ENV-001) 	🟢 Documented — env var not inherited from lunarwing.env when using sudo -u.
🔧 Feature Gaps (Not Bugs — Missing Functionality)
Feature 	Description
FEAT-001 	MCP Server Layer — No stdio Support. Only remote HTTP/SSE endpoints supported. Many MCP servers are stdio-only. (Dwarfstar)
FEAT-002 	MCP tool_auth Defaults to OAuth, No API Key Header Support. Can't inject stored secrets as header credentials. (Dwarfstar)
📡 Watch Items
Item 	Description
WATCH-001 	LLM Streaming/Request Clogging — Provider-specific (TensorZero logs confirm). One tool-calling fix already landed. Ongoing monitoring.
Bug Family Summary

    Gap #1 (internal state → search/context): BUG-002
    Gap #2 (internal state → user surfaces): BUG-001, BUG-003
    Mission/event propagation: BUG-014, BUG-003
    Resource leak: BUG-015, BUG-022 (ghost jobs)
    Context/payload: BUG-020 ✅, BUG-002
    Orchestrator/gate: BUG-018 ✅
    External worker: BUG-023, BUG-022
    Allowlist/config: BUG-004, BUG-005, BUG-007, BUG-008, BUG-012, BUG-019, BUG-024
    Deployment/permissions: BUG-006, BUG-011, BUG-016, BUG-017
    Tool API: BUG-009, BUG-010, BUG-021
    MCP/extension gaps: FEAT-001, FEAT-002
    Logging noise: BUG-013
    LLM/streaming: WATCH-001

Totals: 24 bugs (2 resolved), 2 feature gaps, 1 watch item. 22 still open.