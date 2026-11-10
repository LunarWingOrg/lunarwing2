# Enable Developer Tools for a Tenant

LunarWing controls local filesystem and shell tools through three separate layers:

1. **Registration** decides whether the tools exist.
2. **Sandbox policy** limits filesystem and network access.
3. **Tool permission** decides whether an invocation runs, asks for approval, or is disabled.

**Revalidated against source 2026-11-10 (v2.2.0.0).** `ALLOW_LOCAL_TOOLS`,
`AGENT_AUTO_APPROVE_TOOLS` (including the legacy `AUTO_APPROVE_TOOLS` alias),
the `full_access` double opt-in, the generic podman-first runtime, the four
network tiers, and the scrubbed child environment match the config resolver and
sandbox manager. The Linux network default is now `proxy-enforced`; mt-admin
preserves every explicit operator choice on re-render.

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
| `readonly` | workspace mounted read-only | `proxy-enforced` by default on Linux, `none` elsewhere; configurable | container |
| `workspace_write` | workspace mounted read/write | `proxy-enforced` by default on Linux, `none` elsewhere; configurable | container |
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

Container egress is selected through `SANDBOX_NETWORK`. Since v2.2.0.0 (WI#537), the Linux default is `proxy-enforced`: the container keeps `--network none` and reaches only the validating allowlist proxy through a per-execution Unix-socket relay (public-only destinations, DNS-pinned, redirects rechecked). This requires a local Linux runtime and Python 3 in the image; relay setup fails closed and never degrades to advisory or direct egress. Other platforms default to `none`. Enforced mode installs no credential mappings. Select `none` for no egress, `tunnel` for the item-3 overlay, or `proxy` only as a legacy advisory mode whose ignored env vars permit direct host/network connections. Denied responses carry the stable prefix `sandbox-proxy denied:`. `SANDBOX_ENFORCE=true` forces the sandboxed shell even when dev-mode `ALLOW_LOCAL_TOOLS` would otherwise run it directly on the host.

Other supported startup controls include `SANDBOX_TIMEOUT_SECS`, `SANDBOX_MEMORY_LIMIT_MB`, `SANDBOX_CPU_SHARES`, `SANDBOX_IMAGE`, `SANDBOX_AUTO_PULL`, `SANDBOX_EXTRA_DOMAINS`, `SANDBOX_REAPER_INTERVAL_SECS`, and `SANDBOX_ORPHAN_THRESHOLD_SECS`. Keep additions to the default proxy domain allowlist minimal; the exact shipped set is listed in `ic/src/NETWORK_SECURITY.md` §5. Since v2.1.3.0 (WI#223), mt-admin-provisioned tenants default to `SANDBOX_IMAGE=lunarwing-sandbox:latest` with `SANDBOX_AUTO_PULL=false`; `add-tenant` builds and distributes the image, and `mt-admin build-sandbox-image [tenant]` recovers a missing one — `SANDBOX_AUTO_PULL=true` is a trap for this local-only image (short-name resolution fails without a TTY and there is nothing to pull). WI#537 adds the pinned full Python development suite to that image. mt-admin preserves every operator-set network mode on re-render, including an existing `none` or legacy `proxy`; do not replace an explicit tenant choice during upgrades.

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

Supervised mode (`AGENT_SUPERVISED_MODE=true` in the daemon's process environment) takes precedence and gates every tool action of an Engine V2 foreground thread. The `--supervised` CLI flag has no effect at this revision; see `docs/guides/human-delay-mode.md` for the defect and the scope.

## Recommended Profiles

Workspace-scoped development:

```bash
ALLOW_LOCAL_TOOLS=true
SANDBOX_ENABLED=true
SANDBOX_ENFORCE=true
SANDBOX_POLICY=workspace_write
```

`SANDBOX_ENFORCE=true` is what keeps shell execution in the container here: with `ALLOW_LOCAL_TOOLS=true` and no `SANDBOX_ENFORCE`, the shell tool runs directly on the host and the daemon logs that at startup. With it, shell writes are confined to the workspace and approval prompts are retained. The file tools that `ALLOW_LOCAL_TOOLS` adds (`read_file`, `write_file`, `list_dir`, `apply_patch`) run in the daemon process, not in the container, and the sandbox policy does not confine their paths.

Trusted non-interactive workspace job:

```bash
ALLOW_LOCAL_TOOLS=true
SANDBOX_ENABLED=true
SANDBOX_ENFORCE=true
SANDBOX_POLICY=workspace_write
AGENT_AUTO_APPROVE_TOOLS=true
```

Use only with trusted prompts and inputs. Automatic approval increases prompt-injection impact even when shell access remains workspace-scoped. Without `SANDBOX_ENFORCE=true` this profile runs auto-approved shell commands on the host, and the file tools are host-side in either case.

Avoid this profile unless direct host execution is specifically required:

```bash
ALLOW_LOCAL_TOOLS=true
SANDBOX_POLICY=full_access
SANDBOX_ALLOW_FULL_ACCESS=true
AGENT_AUTO_APPROVE_TOOLS=true
```

## Container storage failures

Since v2.2.0.0 (WI#587), image inspection, image pull, and container setup can
return typed `storage_unavailable` with the extracted containers-storage path
and the runtime's nested error preserved. This is non-transient: check that path
with `df`, then use mt-admin and the host health tooling on multi-tenant hosts.
As the affected tenant user, `podman image prune` removes dangling images only.
Never use `prune -a` or a broad system prune as a first response: the sandbox
image is built locally, not pulled from a registry. Rebuild it with
`build-sandbox-image <tenant>` after cleanup if necessary.

An ordinary command that prints `No space left on device` is not by itself proof
that host container storage is full. Run-path classification requires exit
status 125 together with a stderr line that starts with the runtime's error
prefix (`Error:`), so ordinary command stderr stays ordinary sandbox output.
That shape is not proof of origin: a sandboxed command can exit 125 and print
such a line itself, and its output is then classified as a runtime failure,
storage path included. Confirm with `df` on the host before acting on a
storage error.

## Verification

After the managed restart:

1. Confirm MT admin reports the tenant healthy.
2. Ask the agent to list its tools and verify the local tools are present.
3. Run a harmless read in the workspace.
4. Under `workspace_write`, create and remove a disposable workspace file.
5. Confirm an attempted path outside the workspace is rejected.
6. Confirm approval behavior matches the intended per-user setting.

Do not use production secrets or repositories for this check. Record the active policy and redact environment values before sharing diagnostics.
