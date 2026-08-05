# Enable Developer Tools for a Tenant

LunarWing controls local filesystem and shell tools through three separate layers:

1. **Registration** decides whether the tools exist.
2. **Sandbox policy** limits filesystem and network access.
3. **Tool permission** decides whether an invocation runs, asks for approval, or is disabled.

Use the narrowest access that satisfies the task. `full_access` combined with automatic approval lets model output execute directly on the host with the LunarWing service account's privileges.

## 1. Register Local Tools

Local developer tools are not registered in a normal production configuration unless this startup environment value is enabled:

```bash
ALLOW_LOCAL_TOOLS=true
```

This registers the built-in `read_file`, `write_file`, `list_dir`, `apply_patch`, and `shell` tools. Restart LunarWing after changing it.

For an MT-admin tenant, edit the managed tenant environment file under `/home/<tenant>/lunarwing/env/`, then restart through MT admin. Do not call a tenant's unit directly:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
```

## 2. Select a Sandbox Policy

The default is an enabled read-only container sandbox:

| Policy | Filesystem | Network | Execution |
| --- | --- | --- | --- |
| `readonly` | workspace mounted read-only | proxied allowlist | container |
| `workspace_write` | workspace mounted read/write | proxied allowlist | container |
| `full_access` | unrestricted host access | unrestricted | direct host shell |

For normal development work, prefer:

```bash
SANDBOX_ENABLED=true
SANDBOX_POLICY=workspace_write
```

`full_access` requires a deliberate double opt-in:

```bash
SANDBOX_POLICY=full_access
SANDBOX_ALLOW_FULL_ACCESS=true
```

Without `SANDBOX_ALLOW_FULL_ACCESS=true`, startup conversion logs an error and downgrades `full_access` to `workspace_write`. The sandbox manager also refuses unapproved full-access execution. This policy bypasses Docker entirely; it is not a more permissive container mode.

Other supported startup controls include `SANDBOX_TIMEOUT_SECS`, `SANDBOX_MEMORY_LIMIT_MB`, `SANDBOX_CPU_SHARES`, `SANDBOX_IMAGE`, `SANDBOX_AUTO_PULL`, and `SANDBOX_EXTRA_DOMAINS`. Keep the proxy domain allowlist minimal.

## 3. Keep or Change Approval Behavior

The seeded permission baseline is conservative:

- local file and shell tools default to `ask_each_time`;
- unknown tools also default to `ask_each_time`;
- a small set of read-only or messaging helpers defaults to `always_allow`;
- an explicit `disabled` permission prevents use;
- intrinsically always-gated tools cannot be changed to `always_allow`.

The authenticated web approval card offers **Approve**, **Deny**, and, when the tool permits it, **Always**. The `tool_permission_set` built-in tool can also request a permission change through the approval path. Persisted per-user settings are the source of truth; do not edit the settings table directly for routine administration.

For non-interactive development or CI only, the current global environment override is:

```bash
AGENT_AUTO_APPROVE_TOOLS=true
```

The older `AUTO_APPROVE_TOOLS` spelling is not read by the current configuration resolver. `--auto-approve` sets the same runtime override as `AGENT_AUTO_APPROVE_TOOLS=true`.

Supervised mode (`--supervised` or `AGENT_SUPERVISED_MODE=true`) takes precedence and still gates every tool action.

## Recommended Profiles

Workspace-scoped development:

```bash
ALLOW_LOCAL_TOOLS=true
SANDBOX_ENABLED=true
SANDBOX_POLICY=workspace_write
```

This keeps shell execution in the container, confines writes to the workspace, and retains approval prompts.

Trusted non-interactive workspace job:

```bash
ALLOW_LOCAL_TOOLS=true
SANDBOX_ENABLED=true
SANDBOX_POLICY=workspace_write
AGENT_AUTO_APPROVE_TOOLS=true
```

Use only with trusted prompts and inputs. Automatic approval increases prompt-injection impact even when filesystem access remains workspace-scoped.

Avoid this profile unless direct host execution is specifically required:

```bash
ALLOW_LOCAL_TOOLS=true
SANDBOX_POLICY=full_access
SANDBOX_ALLOW_FULL_ACCESS=true
AGENT_AUTO_APPROVE_TOOLS=true
```

## Verification

After the managed restart:

1. Confirm MT admin reports the tenant healthy.
2. Ask the agent to list its tools and verify the local tools are present.
3. Run a harmless read in the workspace.
4. Under `workspace_write`, create and remove a disposable workspace file.
5. Confirm an attempted path outside the workspace is rejected.
6. Confirm approval behavior matches the intended per-user setting.

Do not use production secrets or repositories for this check. Record the active policy and redact environment values before sharing diagnostics.
