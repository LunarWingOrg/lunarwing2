# Enable Developer Tools for a Tenant

LunarWing controls local filesystem and shell tools through three separate layers:

1. **Registration** decides whether the tools exist.
2. **Sandbox policy** limits filesystem and network access.
3. **Tool permission** decides whether an invocation runs, asks for approval, or is disabled.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-19, 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), and 2026-10-04 (v2.1.5.0).** `ALLOW_LOCAL_TOOLS`,
`AGENT_AUTO_APPROVE_TOOLS`, and the `full_access` double opt-in
(`SANDBOX_ALLOW_FULL_ACCESS`) were re-checked against the config resolver and
sandbox manager. Updated for the v2.1.2.0 generic podman-first sandbox: new
`SANDBOX_RUNTIME` / `SANDBOX_NETWORK` / `SANDBOX_ENFORCE` knobs and the
scrubbed `full_access` child environment (WI#141). The 2026-09-19 pass
confirmed the approval-behavior section against the goal-31 fixes
(`AUTO_APPROVE_TOOLS` legacy alias, Engine V2 `UnlessAutoApproved` scope).
The 2026-09-27 repass added the v2.1.3.0 mt-admin sandbox-image defaults
(WI#223: `SANDBOX_IMAGE=lunarwing-sandbox:latest`, `SANDBOX_AUTO_PULL=false`,
`SANDBOX_RUNTIME=podman` for new tenants) and the env-generation note that
mt-admin now writes only the canonical `AGENT_AUTO_APPROVE_TOOLS` key for new
tenants, migrating a legacy-only `AUTO_APPROVE_TOOLS` value on re-render.

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

The default is an enabled read-only container sandbox. Since v2.1.2.0 the sandbox is generic and podman-first: it drives the container runtime CLI directly (no daemon socket required, so rootless podman and OpenRC hosts work), preferring podman and falling back to docker. Select the runtime explicitly with `SANDBOX_RUNTIME=auto|podman|docker`.

| Policy | Filesystem | Network | Execution |
| --- | --- | --- | --- |
| `readonly` | workspace mounted read-only | `none` by default; configurable | container |
| `workspace_write` | workspace mounted read/write | `none` by default; configurable | container |
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

Without `SANDBOX_ALLOW_FULL_ACCESS=true`, startup conversion logs an error and downgrades `full_access` to `workspace_write`. The sandbox manager also refuses unapproved full-access execution. This policy bypasses the container entirely; it is not a more permissive container mode.

Even under `full_access`, the child environment is scrubbed: commands inherit only the allowlisted OS/toolchain variables (the same list the unsandboxed shell tool uses), never daemon credentials such as `DATABASE_URL` or the `SANDBOX_*` launcher variables themselves.

Container egress is selected through `SANDBOX_NETWORK`: `none` (default) passes `--network none` for full network isolation — no egress at all, including loopback. For web-capable sandboxes choose `proxy-enforced`: the container keeps `--network none` and reaches only the validating allowlist proxy through a per-execution Unix-socket relay (public-only destinations, DNS-pinned, redirects rechecked). This requires a local Linux runtime and Python 3 in the image; relay setup fails closed (it never degrades to advisory or direct egress). `proxy` remains as the legacy advisory mode, in which commands can ignore proxy env vars and connect directly to the host/network. Denied responses carry the stable prefix `sandbox-proxy denied:`. `SANDBOX_ENFORCE=true` forces the sandboxed shell even when dev-mode `ALLOW_LOCAL_TOOLS` would otherwise run it directly on the host.

Other supported startup controls include `SANDBOX_TIMEOUT_SECS`, `SANDBOX_MEMORY_LIMIT_MB`, `SANDBOX_CPU_SHARES`, `SANDBOX_IMAGE`, `SANDBOX_AUTO_PULL`, `SANDBOX_EXTRA_DOMAINS`, `SANDBOX_REAPER_INTERVAL_SECS`, and `SANDBOX_ORPHAN_THRESHOLD_SECS`. Keep the proxy domain allowlist minimal. Since v2.1.3.0 (WI#223), mt-admin-provisioned tenants default to `SANDBOX_IMAGE=lunarwing-sandbox:latest` with `SANDBOX_AUTO_PULL=false`; `add-tenant` builds and distributes the image, and `mt-admin build-sandbox-image [tenant]` recovers a missing one — `SANDBOX_AUTO_PULL=true` is a trap for this local-only image (short-name resolution fails without a TTY and there is nothing to pull). Upgrade note (WI#154): the default `SANDBOX_NETWORK` is `none` everywhere in v2.1.2.0 — an explicit `proxy` setting is preserved, but sandboxed web access now requires an explicit choice of `proxy-enforced` (recommended) or the legacy advisory `proxy`. See [WI#179 combined rollout](../ops/history/v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md).

## 3. Keep or Change Approval Behavior

The seeded permission baseline is conservative:

- local file and shell tools default to `ask_each_time`;
- unknown tools also default to `ask_each_time`;
- a small set of read-only or messaging helpers defaults to `always_allow`;
- an explicit `disabled` permission prevents use;
- intrinsically always-gated tools ignore persistent `always_allow` grants unless
  the tool explicitly opts in, as built-in `ssh` does.

`secret_delete` never permits **Always**: approve each irreversible deletion
individually. Global/session auto-approval cannot bypass it. Upgrade migration
V28 removes older persistent deletion grants for every user, preserving
`disabled` and `ask_each_time`; a downgrade does not restore those grants.
Both secrets tools (`secret_list`, `secret_delete`) are interactive-only.

The authenticated web approval card offers **Approve**, **Deny**, and, when the tool permits it, **Always**. A persisted `always_allow` row for `secret_delete`, however created, is ignored by every approval path. The generic settings API/import can still write such a row; enforcement is at execution, not a settings-write lock. Do not edit the settings table directly for routine administration.

For non-interactive development or CI only, the current global environment override is:

```bash
AGENT_AUTO_APPROVE_TOOLS=true
```

The older `AUTO_APPROVE_TOOLS` spelling is accepted as a legacy alias, with a
name-only migration warning. Prefer `AGENT_AUTO_APPROVE_TOOLS`; it wins when both
spellings are set in the same configuration layer. Existing precedence across
environment, runtime overrides, injected settings, and DB defaults is preserved.
`--auto-approve` sets the canonical runtime override.

Engine V2 uses the resolved setting as well as V1. Global auto-approval bypasses
only `UnlessAutoApproved` tools: disabled permissions, intrinsic `Always` gates,
shared-conversation restrictions, and supervised mode still apply. Unknown
`AGENT_*` / `AUTO_APPROVE_*` keys produce a warning containing only the key name,
so a misspelled approval setting does not silently look enabled.

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
