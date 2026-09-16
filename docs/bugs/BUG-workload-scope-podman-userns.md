# Workload-scope podman userns join failure (WI#142)

> **Status: FIX-LANDED (ops tooling + prevention; 2026-09-16).** `fix-podman-userns`
> repairs a broken tenant userns pairing at runtime, pause.pid hygiene is shared
> by provisioning and repair, and provisioning pre-warms the pairing from a
> clean operator context. The structural launcher-side fix (manager-forked
> transient services instead of daemon-forked scopes) is deliberately NOT taken
> this round — see "Why the launcher-side fix is not taken (yet)".

## Symptom

With the 2.1.2.0 workload sandbox (agent tool shells spawned as systemd
transient scopes via `systemd-run --user --scope` in
`lunarwing-workloads.slice`, see `ic/src/tools/process.rs`), every `podman`
invocation from an agent shell fails with:

```
Error: cannot re-exec process to join the existing user namespace: ... Try running "podman system migrate" ...
```

The tenant's own sidecar containers (pg, vision, voice — Quadlet-managed) keep
running; only CLI invocation from the agent context fails. First seen on a
Fedora 43 host (podman 5.8.4, tenant uid 1009, scope
`user-1009.slice/lunarwing.slice/lunarwing-workloads.slice/run-p*.scope`);
same error signature as the earlier voice-sidecar userns episode and the
v1.1.4 OpenRC O1 fault (`docs/bugs/history/OPENRC-MT-1.1.4-ISSUES.md`).

## Root cause chain

`podman ps` (any rootless podman command) runs `SetupRootless`
(`podman/pkg/domain/infra/abi/system_linux.go`), which joins the existing
rootless userns via the pause process (`/run/user/<uid>/libpod/tmp/pause.pid`)
or, failing that, the running conmon pid files (`TryJoinFromFilePaths`). The
join (`reexec_userns_join`, `podman/pkg/rootless/rootless_linux.c`) opens
`/proc/<pid>/ns/{user,mnt}`, forks, `setns()`s both, and re-execs podman. The
observed error is emitted by `joinUserAndMountNS`
(`podman/pkg/rootless/rootless_linux.go`) when that helper returns <0, and the
`"...Try running 'podman system migrate'"` wrapper comes from the caller in
`system_linux.go`.

Kernel-side, `setns(CLONE_NEWUSER)` into a child userns requires
`ns_capable(target, CAP_SYS_ADMIN)`, which `cap_capable_helper`
(`security/commoncap.c`) grants to the owner of the userns' top-level child
(`ns->parent == cred_ns && ns->owner == euid`) — and `setns(CLONE_NEWNS)`
additionally requires `CAP_SYS_ADMIN` in the userns that **owns the target
mount namespace**. The join therefore fails when the pause/conmon processes
live in a pairing the invoking context cannot rejoin:

- **Foreign/stale state after uid reuse.** The integration-testing cadence
  nukes tenants routinely; `/run/user/<uid>` is created by `install -d`, not
  by a login session, so logind never reaps it (the O1 fault). The purge path
  reaps it today, but any state that survives a non-purge removal or a manual
  teardown leaves the next tenant a dead or foreign-owned pairing.
- **Foreign euid creation.** Any podman run against tenant dirs under a
  different euid (host-side debugging — the voice-sidecar episode) creates a
  userns whose owner is not the tenant; the tenant can then never join it
  (`cap_ptrace_access_check` denies opening its ns files; `ns_capable` denies
  the setns).
- **Context-crossing joins across the workload-scope boundary.** A podman
  command that runs *inside a workload scope* is a fork of the daemon's own
  child: it inherits the daemon unit's `NoNewPrivileges=true` and
  `PrivateTmp=true` mount namespace (an init-owned private-/tmp mount ns) —
  `systemd-run --scope` forks the caller; the manager does not re-exec it.
  Podman state created in that context (a pause process re-created after a
  crash, conmons of agent-started containers) is bound to a mount namespace
  that a plain session context cannot `setns` into, and vice versa. Once the
  pairing is crossed, every join attempt from the "other" context fails —
  which is why the failure looks context-specific ("works outside the scope")
  even though the uid is the same.

## Why the launcher-side fix is not taken (yet)

The issue's option 2 — launch the workload so it shares the podman stack's
context — is the structural fix, but it is not implementable within the
current scope design:

- `NoNewPrivileges` and the private-tmp mount namespace are **fork-inherited**
  from the daemon; no `--property` on a scope clears them (scopes do not set
  up namespaces; the process is already forked by `systemd-run`).
- The clean alternative is a manager-forked transient **service** (the same
  context class as the Quadlet units, where podman demonstrably works). That
  changes the lifecycle contract: service environments come from the manager,
  not the caller, so the scrubbed env + injected secrets would have to move to
  `--setenv` (leaks secrets via argv) or per-command `EnvironmentFile=` state,
  and the kill tree must switch from process-group signals to
  `systemctl --user stop <unit>`. That is a redesign of
  `ic/src/tools/process.rs` lifecycle semantics (BUG-005), not a scoped fix.

## The landed fix (ops tooling)

1. **`fix-podman-userns <tenant>`** (`ic/scripts/lunarwing-mt-admin.sh`,
   `repair_tenant_podman_userns` in `ic/scripts/mt-admin-lib/04-tenant-lifecycle.sh`):
   probe as the tenant (`podman ps` via `_ctr`); if healthy, a no-op. On the
   join-failure signature:
   `stop-tenant` (kills the conmons holding the broken pairing via the init
   system — tenant podman cannot stop them, it fails with the same join
   error) → pause.pid hygiene → `podman system migrate` as the tenant
   (containers already stopped) → `start-tenant` (recreates pg/vision/voice/
   daemon/workers health-gated in a clean, tenant-owned pairing) → verify.
   The classifier (`_podman_userns_join_failure`) matches the bare join
   error, the `failed to reexec: Permission denied` variant, and the
   system-migrate wrapper; any other podman failure gets only the cheap
   pause.pid cleanup and then fails loudly without stopping the tenant.
2. **Shared pause.pid hygiene** (`clean_stale_pause_pid`,
   `ic/scripts/mt-admin-lib/05-tenant-build.sh`): extracted from
   `ensure_rootless_prereqs` so provisioning and the repair verb use the same
   dead-pid / foreign-owner rule (live tenant-owned pause processes are kept —
   the add-tenant resume path).
3. **Provision-time pre-warm** (`ensure_rootless_prereqs`): after
   `podman system migrate` (which leaves no pause process), a best-effort
   `podman ps` as the tenant from the operator context creates the userns
   pairing in a clean, canonical context, so the tenant's pairing is never
   born inside the first agent's workload scope.

## Operator guidance

- When an agent reports the join error, run
  `sudo ic/scripts/lunarwing-mt-admin.sh fix-podman-userns <tenant>`; it is
  safe on a healthy tenant (probe-gated no-op).
- Never run podman against a tenant's dirs as root or another euid; use
  mt-admin's `_ctr` paths (or `sudo -u <tenant> env HOME=... XDG_RUNTIME_DIR=...`).
- After killing a tenant's processes outside mt-admin (manual debugging),
  re-run the repair verb or clean `/run/user/<uid>/libpod/tmp/pause.pid`
  before the tenant's podman is used again.
- Rebooting the host or the tenant user manager also resets the pairing
  (the issue's original workaround), at the cost of a tenant restart.

## Verification

- `ic/scripts/tests/test-mt-admin-fix-podman-userns.sh`: probe-gated no-op,
  stop→cleanup→migrate→start ordering, stale/dead/foreign/keep pause.pid
  branches, non-join failures never stop the tenant, failing migrate
  tolerated, classifier signatures.
- `ic/scripts/check-mt-admin.sh` (shellcheck over root + all modules) green.
