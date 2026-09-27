# Supervised Mode

Supervised mode is LunarWing's global human-in-the-loop setting for tool execution. When enabled, every tool action requires explicit approval, regardless of the tool's normal approval tier or an auto-approve setting.

The older "Human Delay Mode" phase documents referenced by this file no longer exist. The implemented feature is named **supervised mode** in the CLI, runtime configuration, thread state, Engine V2 gate context, and effect adapter.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0).** The `--supervised` CLI
flag and `AGENT_SUPERVISED_MODE` runtime override were re-checked against
`ic/src/main.rs` and the bridge router; no changes were needed.

## Enable It

Start LunarWing with the CLI flag:

```bash
lunarwing --supervised
```

Or set the startup environment value:

```bash
AGENT_SUPERVISED_MODE=true lunarwing
```

The CLI flag installs the same runtime configuration override. Enable it in the service environment when supervised behavior must persist across service restarts.

## Behavior

- New thread configuration is marked `supervised_mode` when `AGENT_SUPERVISED_MODE` is `true` or `1`.
- Every proposed tool action is sent through the approval path, including tools whose normal policy would not require approval.
- Supervised mode takes precedence over global auto-approve behavior.
- Approving an action authorizes that resolved action; it does not disable supervised mode for later actions.
- Rejections return through the normal tool-result path so the agent can respond without running the rejected action.

Approvals can be handled through the active channel or authenticated web experience according to the deployment's normal approval flow. Do not treat an approval request as proof that an action already ran.

## Operational Notes

Supervised mode is a safety control, not a substitute for least-privilege tool policies, sandboxing, or channel authentication. Operators should still restrict dangerous tools and protect approval channels.

To verify the active startup configuration, inspect the service definition without exposing secret-bearing environment values, then run a harmless tool request and confirm it pauses for approval. On multi-tenant hosts, use `ic/scripts/lunarwing-mt-admin.sh status <tenant>` for tenant diagnostics rather than bare system-bus `systemctl`.
