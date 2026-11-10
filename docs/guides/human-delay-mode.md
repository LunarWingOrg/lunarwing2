# Supervised Mode

Supervised mode is LunarWing's human-in-the-loop setting for tool execution. When enabled, every tool action of an Engine V2 foreground thread requires explicit approval, regardless of the tool's normal approval tier or an auto-approve setting. See [Scope](#scope) for what it does not cover.

The older "Human Delay Mode" phase documents referenced by this file no longer exist. The implemented feature is named **supervised mode** in the CLI, runtime configuration, thread state, Engine V2 gate context, and effect adapter.

**Revalidated against source 2026-11-10 (v2.2.0.0).** `AGENT_SUPERVISED_MODE`
in the daemon's process environment is the working switch: the bridge router
reads it when it creates a foreground thread. The `--supervised` CLI flag does
**not** enable supervised mode at this revision (see below). Where it applies,
supervised mode takes precedence over global auto-approval.

## Enable It

Set the value in the daemon's process environment:

```bash
AGENT_SUPERVISED_MODE=true lunarwing
```

`true`, `1`, `yes` and `on` are accepted. For a service, set it in the service
environment so it persists across restarts. A `.env` file in the daemon's
working directory or in the LunarWing base directory also works, because both
are loaded into the process environment at startup.

> **Known defect: `lunarwing --supervised` has no effect.** The flag stores an
> in-process configuration override (`set_runtime_env` in `ic/src/main.rs`), but
> the bridge router reads the process environment (`std::env::var` in
> `ic/src/bridge/router.rs`), and nothing copies the override there. A daemon
> started with only the flag is not supervised, and it logs no warning. Use the
> environment variable until the flag is fixed.

## Scope

- Only Engine V2 foreground threads carry the setting.
- Mission-spawned threads are not supervised: they are built from
  `ThreadConfig::default()`. One approved `mission_create`, `mission_fire` or
  `mission_resume` starts runs that execute without per-action approval (see
  the mission sections of `docs/architecture/ENGINE-V2.md`).
- The legacy V1 agent loop under `ic/src/agent/` has no supervised handling.
  Channels that are still served by V1 are not gated by this setting.

## Behavior

- New Engine V2 foreground thread configuration is marked `supervised_mode` when `AGENT_SUPERVISED_MODE` is `true`, `1`, `yes` or `on` in the process environment.
- Every tool action such a thread proposes is sent through the approval path, including tools whose normal policy would not require approval.
- Supervised mode takes precedence over global auto-approve behavior.
- Approving an action authorizes that resolved action; it does not disable supervised mode for later actions.
- Rejections return through the normal tool-result path so the agent can respond without running the rejected action.

Approvals can be handled through the active channel or authenticated web experience according to the deployment's normal approval flow. Do not treat an approval request as proof that an action already ran.

## Operational Notes

Supervised mode is a safety control, not a substitute for least-privilege tool policies, sandboxing, or channel authentication. Operators should still restrict dangerous tools and protect approval channels.

To verify the active startup configuration, inspect the service definition without exposing secret-bearing environment values, then run a harmless tool request and confirm it pauses for approval. On multi-tenant hosts, use `ic/scripts/lunarwing-mt-admin.sh status <tenant>` for tenant diagnostics rather than bare system-bus `systemctl`.
