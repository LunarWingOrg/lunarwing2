External `create_job` calls default to no conversation history. Set
`context_scope: "recent"` to send bounded user/assistant messages from current
thread as untrusted reference context; task description remains authoritative.

Local and Docker `create_job` workers default to 200 tool-call iterations. Set
`max_iterations: 300` (up to 500) for large multi-fix write tasks. Named
external workers manage their own iteration policy and reject this option.