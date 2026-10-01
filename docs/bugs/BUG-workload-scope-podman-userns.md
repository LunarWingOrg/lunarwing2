# Workload-scope podman userns join failure (WI#142 → WI#178/WI#181)

> **Status: STRUCTURALLY FIXED (2026-09-19, WI#181).** The launcher-side fix
> this doc previously deferred — manager-forked transient **services** instead
> of daemon-forked scopes — is now landed in `ic/src/tools/process.rs`.
> Workloads spawn as `systemd-run --user --wait --pipe` transient services
> (forked by the user manager, the same context class as the tenant's Quadlet
> units where podman demonstrably works), with the scrubbed environment and
> working directory handed over via `EnvironmentFile=` (mode 0600, deleted
> after the run) and `--working-directory=`. Timeout/cancel stops the unit via
> `systemctl --user stop` before the process-group kill. The older
> `fix-podman-userns` repair verb remains as the operator-side recovery path
> for pairings already broken before this fix (e.g. by foreign-euid podman
> runs).
>
> **2026-09-28 follow-up:** Goal 12 found a separate startup-probe trigger on
> Podman 6.1.2. The direct reaper scan/cleanup now use the isolated workload
> launcher, and client-version probes use the `version` subcommand; the live
> reproduction and repair limitation are described below.

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

## Historical limitation of the original scope design

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

## The structural fix (landed 2026-09-19, WI#181)

The issue's option 2 — launch the workload so it shares the podman stack's
context — is now implemented in `ic/src/tools/process.rs`:

- `WorkloadSpawner` builds `systemd-run --user --wait --pipe --quiet
  --collect` invocations with a generated `--unit=` name inside
  `lunarwing-workloads.slice` (same slice, same cgroup accounting as the old
  scopes), plus `--property=TimeoutStopSec=` and the optional `MemoryMax=`.
- `prepare_service()` re-marshals the caller's scrubbed environment into a
  mode-0600 `EnvironmentFile=` (values double-quoted; unrepresentable values
  skipped) and the working directory into `--working-directory=` — the
  daemon's scrubbed env never crosses argv and never reaches the manager by
  inheritance. The rebuilt invocation also carries
  `--property=UnsetEnvironment=*` (list syntax matches every variable
  inherited from the user manager), so the env file is the unit's EXACT
  environment: a manager-borne credential can never leak into a workload
  the caller did not allowlist (MR !1042 review).
- `ProcessTreeGuard` gains the transient unit: `terminate()`/`kill_tree()`
  stop the unit via `systemctl --user stop` (SIGTERM, then SIGKILL after
  `TimeoutStopSec`) before signaling the process group. `Drop` — which
  cannot await, and where the manager-forked workload outlives the killed
  `systemd-run` client — hands the unit to a process-wide background stop
  queue (bounded by `LUNARWING_WORKLOAD_DROP_STOPS`) that runs every stop
  to completion; `finish()` is the cancellation-safe await for call sites
  that dropped an execution future. `--wait` proxies stdio and the
  service's exit status, so callers keep their normal spawn/wait shape.
- All four workload call sites (`shell.rs` direct path, sandbox
  `execute_direct`, sandbox `cli_runner` podman CLI, registry WASM builds)
  use `prepare_service` + `for_service`, and the podman lifecycle
  operations that `run` is not the only caller of — `image inspect`,
  `image pull`, and the timeout/cleanup `kill`/`rm` — go through the same
  isolated context (MR !1042 review), so `SandboxManager::initialize()`
  and cleanup never fail on the hardened-daemon context.

Because the workload is forked by the user manager, it never inherits the
daemon's `NoNewPrivileges=true` / `PrivateTmp=true` context — the exact
context-crossing that broke podman's `setns` join. Verified on a systemd 261
host: `podman ps` from a transient service with `NoNewPrivileges=true` works
(exit 0), while the daemon-forked scope shape is the documented failure.

## Remaining direct startup/runtime calls (Goal 12, 2026-09-28)

Fresh real-web provisioning on Podman **6.1.2** reproduced a separate failure:
the daemon stayed active, but the runtime could no longer see PostgreSQL and
both selected workers failed to start. Its error named a missing `conmon.pid`
and recommended `podman system migrate`.

Both `check_podman()` and `probe_podman_version()` directly invoked
`podman --version`. In Podman 6.1.2's `pkg/rootless/rootless_linux.c`, that flag
still enters the rootless constructor. Under the daemon's
`NoNewPrivileges=true` / `PrivateTmp=true` context, inspecting the live pause
process fails; `check_stale_pause_pid()` treats the unreadable process as a
recycled PID and unlinks its valid tracking file. The command still exits 0.
The `version` **subcommand** is explicitly excluded by `can_use_shortcut()`.

A controlled test under the test tenant's hardened user service reproduced:

| Probe | Exit | Existing live `pause.pid` preserved |
| --- | --- | --- |
| `podman --version` | 0 | **No** |
| `podman version --format '{{.Client.Version}}'` | 0 | **Yes** |

The two client probes now share `PODMAN_VERSION_ARGS`, using the subcommand
with `podman version {{.Client.Version}}` formatting to retain the existing
version parser. Deploying that fix alone did not resolve startup: the
socketless sandbox reaper still directly launched `podman ps` on its immediate
first tick (and `rm` during cleanup), bypassing the workload launcher. These
calls now use `WorkloadSpawner` and `ProcessTreeGuard`, just like execution and
image operations. The five-second command timeout remains; timeout cleanup
awaits stopping the transient service/process tree. OpenRC retains the shared
process-group fallback. Label filters, orphan-age checks, in-flight protection,
and the separate job-container sweep are preserved.

Daemon hardening remains intact. Regressions exercise output capture and exit
status in both direct and detected-isolation contexts, and ensure a timed-out
command cannot leave its descendant running.

The already-broken test runtime required a narrow manual intervention:
`system migrate` itself could not recover with the missing file and stale
conmon metadata. Under the tenant lifecycle lock, the test identified exactly
one tenant-owned live `catatonit` process with `_PODMAN_PAUSE=1`, restored its
missing tracking file, and then used ordinary mt-admin stop/start. No container
store or database was deleted. The generic repair verb still fails closed if
migration cannot restore runtime introspection; do not claim it automatically
recovers this missing-file case.

See [the real-web acceptance record](../ops/GOAL12_REAL_WEB_ACCEPTANCE.md) for
post-deployment worker/export/restart evidence.

## The repair path (kept for already-broken pairings)

1. **`fix-podman-userns <tenant>`** (`ic/scripts/lunarwing-mt-admin.sh`,
   `repair_tenant_podman_userns` in `ic/scripts/mt-admin-lib/04-tenant-lifecycle.sh`):
   probe as the tenant (`podman ps` via `_ctr`); if healthy, a no-op. On the
   join-failure signature:
   stop init-managed services/supervisors → pause.pid hygiene →
   `podman system migrate` as the tenant (stops remaining containers and resets
   the pairing) → full `stop-tenant` with verified writer quiescence →
   `start-tenant` (recreates pg/vision/voice/
   daemon/workers health-gated in a clean, tenant-owned pairing) → verify.
   Goal 12 acceptance exposed a circular dependency in the earlier sequence:
   the full `stop-tenant` correctly rejected broken runtime introspection, so
   repair never reached migration. The repair-only preliminary stop now uses
   the existing systemd-user/OpenRC init helpers. A service-stop failure blocks
   migration; systemd transitional services are stopped and an unclassifiable
   native service probe also blocks repair. Failed or still-unclassifiable
   writer state after migration blocks restart. Ordinary stop/export retain their
   full runtime-backed proof and
   never receive a quiescence bypass. A failed repair can leave the test/target
   tenant stopped; inspect its state before retrying.
   The classifier (`_podman_userns_join_failure`) matches the bare join
   error, the `failed to reexec: Permission denied` variant, and the
   system-migrate wrapper; any other podman failure gets only the cheap
   pause.pid cleanup and then fails loudly without stopping the tenant.
   Since WI#271, every success path (probe-gated no-op, pause.pid-only
   recovery, and the full cycle) also audits the tenant store for expected
   worker/sidecar images and WARNs with the recovery verbs — a green probe no
   longer concludes the repair while the store is missing an image (the
   distribution gate in `start-tenant` warns and silently skips).
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

- Goal 12 live rootless-Podman acceptance: the hardened daemon reaped an old
  labelled sandbox fixture, preserved an unlabelled control, and retained valid
  pause metadata. PostgreSQL, Pebble, and OpenCode remained usable through real
  encrypted export and restart. The version-probe comparison above reproduced
  the destructive flag form separately.
- Reaper Rust unit tests: **23 passed**, including output/exit status through
  direct/detected isolation and timeout cleanup of descendants.
- `ic/scripts/tests/test-mt-admin-fix-podman-userns.sh`: probe-gated no-op,
  init-stop→cleanup→migrate→verified-stop→start ordering, stale/dead/foreign/keep pause.pid
  branches, non-join failures never stop the tenant, failing migrate
  tolerated, classifier signatures, and (WI#271) tenant-store image-drift
  reporting on both the healthy-probe and full-repair success paths.
- `ic/scripts/tests/test-mt-admin-podman-repair-quiescence.sh` (also invoked by
  the repair test): real repair/stop/quiescence composition with a runtime that
  cannot answer until migration, on both init paths. Covers service-stop
  failure, unknown/transitional systemd states, persistent runtime failure,
  surviving writers, stale deselected OpenRC supervisors, and ordinary
  stop/export remaining fail-closed.
- `ic/scripts/check-mt-admin.sh` (shellcheck over root + all modules) green.
