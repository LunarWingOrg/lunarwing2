# External Worker Conversation History + Iteration Budget

> **Current status (reverified 2026-08-26, rev `9f7012b4`): IMPLEMENTED.**
> Both behaviors below match current source: `context_scope` and
> `max_iterations` handling live in `ic/src/tools/builtin/job.rs`;
> `DEFAULT_WORKER_ITERATIONS` is 200 and `MAX_WORKER_ITERATIONS` is 500
> (`ic/crates/lunarwing_common/src/lib.rs:20-23`), and named external workers
> reject `max_iterations` outright.

External `create_job` calls default to no conversation history. Set
`context_scope: "recent"` to send bounded user/assistant messages from current
thread as untrusted reference context; task description remains authoritative.

Local and Docker `create_job` workers default to 200 tool-call iterations. Set
`max_iterations: 300` (up to 500) for large multi-fix write tasks. Named
external workers manage their own iteration policy and reject this option.