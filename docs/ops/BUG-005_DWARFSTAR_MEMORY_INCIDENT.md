# BUG-005: Dwarfstar memory incident investigation

> **Status:** RECLASSIFIED — resource-unbounded local build and subprocess-lifecycle failure
>
> **Identifier scope:** This report's `BUG-005` label is not the unrelated
> `BUG-005` (“Multica Daemon Connection Refused”) in
> `docs/ops/SUNBURST_BUG_LIST.md`.
>
> **Incident:** 2026-08-01/2026-08-02, tenant `dwarfstar`
>
> **Tenant worktree revision checked out before the incident:**
> `5b50796626017f24710998394e6c38b747f98e38`
>
> The running binary's embedded source revision was not independently recorded.
>
> **Investigation baseline:** `memleakfix4/cmc/v2.0.4.0`, commit `6c82e8c`;
> the report itself was added one commit later as `11b673d`.
>
> **Cross-verification:** Rechecked against source baseline `1e319bf` on
> 2026-08-02. The timeout, process-tree, and unit-resource findings remain
> current. Cross-verification corrected the original interpretation of
> `mode=worker`: current source deliberately routes it to the local scheduler
> when no container job manager exists, and describes that backend as local.
> Historical tenant journal, database, filesystem, and `/proc` figures below are
> preserved investigation evidence; they cannot be independently reproduced
> from this checkout alone.
>
> **Confidence:** High that Rust compilation drove the observed service-cgroup
> peaks; no evidence confirms a Monty, Wasmtime, or LunarWing-process heap leak.

## Executive conclusion

The reported `34.8G` and `37.6G` values were not measurements of the LunarWing
main process's RSS. They were systemd summaries of peak memory charged to the
complete `lunarwing-dwarfstar.service` cgroup.

The strongest evidence identifies Rust compilation as the dominant workload:

- The persisted local job `775d1565-a08b-4e83-afb7-023a99c99232` invoked
  `cargo test` through the built-in `shell` tool.
- Both initial Cargo calls hit the generic 60-second tool timeout.
- Follow-up actions continued to observe Cargo processes well after those tool
  calls had timed out.
- During the second service invocation, 1,844 files were written or modified
  under Dwarfstar's `target/debug` tree. Their aggregate final size was
  approximately 2.91 GB. They include Cargo fingerprints, incremental dependency
  graphs, Rust object files, integration-test binaries, and in-progress compiler
  artifacts. This figure is artifact size, not measured write traffic or RSS.
- The second service invocation lasted about 114 seconds but accumulated about
  14 CPU-minutes, equivalent to roughly 7.4 CPU cores running continuously.
  That profile is consistent with parallel `rustc` processes and inconsistent
  with an idle 150 MB daemon.

The incident is therefore best classified, at high confidence, as:

> A resource-unbounded Rust build ran through LunarWing's available local
> worker/shell path under, or charged to, the LunarWing service cgroup. That path
> lacked reliable process-tree cleanup on timeout/cancellation.

The cgroup did not itself leak memory: systemd correctly reported aggregate
unit usage. Exact peak attribution was not sampled, so the compiler/page-cache
split remains inferred rather than measured directly. The source-backed product
defects are the missing build-specific resource boundary, inconsistent timeout
ownership, and incomplete process-tree cleanup around local shell work. The
evidence does not establish a sandbox escape: local execution was an intentional
backend when Docker was unavailable.

Commit `d2ff7b8` fixes a separate, real Engine V2 output-boundary bug. It should
be retained, but it is not sufficient to explain or fix this incident.

## Incident corrections

Two descriptions in the original incident transcript were inaccurate.

### `37.6G` was not proven process RSS

The journal line was:

```text
lunarwing-dwarfstar.service: Consumed 14min 209ms CPU time,
37.6G memory peak, 1.8G memory swap peak.
```

This is systemd unit/cgroup accounting. It is not `/proc/<lunarwing-pid>/status`
`VmRSS` or `VmHWM`.

### The second run lasted 114 seconds, not 14 wall-clock minutes

The second invocation was:

```text
23:24:03-04:00  service started
23:25:57-04:00  service stopped
```

The `14min 209ms` field is accumulated CPU time across all tasks charged to the
unit. Dividing that CPU time by the approximately 114-second wall duration gives
about 7.4 cores of sustained work.

## Reconstructed timeline

Times below use the host's local offset (`-04:00`). Internal application and
PostgreSQL records use UTC; UTC is four hours later.

| Local time | Evidence |
|---|---|
| 23:04:50 | Job `775d1565` was created with `source=direct`, `job_mode=worker`. |
| 23:05:57 | Job action 0 ran `cargo test --test e2e_reflex_compiler`; it failed at the 60-second tool timeout. |
| 23:07:01 | Job action 1 ran `cargo test --lib reflex::tests`; it also timed out. |
| 23:12–23:23 | Later shell actions repeatedly checked active Cargo processes and tailed `/tmp/e2e_reflex_test.log`. |
| 23:21:24 | Journal recorded `Tool shell timed out after 60s` for job `775d1565`. |
| 23:23:23 | First daemon invocation stopped: `34.8G` cgroup memory peak, `2G` swap peak, `1h43m31s` CPU. |
| 23:24:03 | Dwarfstar daemon restarted. |
| 23:25:09 | Broad writes began in `/home/dwarfstar/lunarwing/ic/target/debug`. |
| 23:25:09–23:25:57 | 1,844 target files were written or modified; their aggregate final size was about 2.91 GB. |
| 23:25:57 | Second daemon invocation stopped: `37.6G` cgroup memory peak, `1.8G` swap peak, `14m209ms` CPU. |
| 23:26:51 | Dwarfstar started again and subsequently returned to a normal footprint. |

The exact launcher continuity across the first restart cannot now be proven; see
[Remaining uncertainty](#remaining-uncertainty). The filesystem and CPU evidence
does prove that a broad Rust test compilation was active during the second peak.

## Evidence collected

### 1. Service-cgroup accounting

The live service cgroup is:

```text
/user.slice/user-1008.slice/user@1008.service/app.slice/
  lunarwing-dwarfstar.service
```

The Linux cgroup-v2 memory controller defines `memory.current` as total memory
used by a cgroup and descendants. Its accounting includes major user and kernel
memory categories, including anonymous memory, file/page cache, kernel data
structures, and socket buffers. A forked process begins in its parent's cgroup
unless explicitly migrated.

Consequently, the normal host-local ancestry:

```text
lunarwing
  └─ sh -c <command>
       └─ cargo
            ├─ rustc
            └─ linker
```

is charged to `lunarwing-dwarfstar.service` unless another launcher moves it to
a separate cgroup.

A later healthy snapshot showed the distinction clearly:

- Main service `memory.current`: approximately 140 MiB.
- Main service `memory.peak`: approximately 165 MiB for that invocation.
- Main LunarWing process `VmRSS`/`VmHWM`: normal hundreds-of-MiB range.
- No `cargo`, `rustc`, or linker process was active in the service.
- OpenCode and Kimi were in sibling service cgroups rather than the main daemon
  cgroup.

This healthy state does not prove the absence of all cumulative retention, but
it proves that the earlier 37.6 GB value was not the daemon's normal resident
footprint.

Reference: [Linux cgroup v2 documentation](https://docs.kernel.org/admin-guide/cgroup-v2.html).

### 2. Direct build-artifact evidence

A read-only timestamp scan of Dwarfstar's build tree for the exact second
invocation found:

```text
first write: 2026-08-01 23:25:09.007055576 -04:00
last write:  2026-08-01 23:25:57.477985406 -04:00
files:       1,844
bytes:       2,912,596,004
```

Representative artifacts included:

- `.fingerprint/lunarwing-*/invoked.timestamp`
- `incremental/*/dep-graph.bin`
- `incremental/*/query-cache.bin`
- `deps/*.rcgu.o`
- integration-test binaries
- `*-working/dep-graph.part.bin`

The names cover many test targets, including worker, gateway, safety, trace,
workspace, XMPP/WASM, and OpenClaw integration tests. This was a broad Rust test
compilation, not merely LunarWing loading prebuilt code.

### 3. Persisted job evidence

The implicated job was persisted as:

```text
id:       775d1565-a08b-4e83-afb7-023a99c99232
source:   direct
job_mode: worker
status:   cancelled
```

For direct scheduler jobs, `save_job()` persists `source=direct` but omits the
`job_mode` column, so the schema default supplies `worker`. That value does not
prove a Docker backend was requested or used; `source=direct` and the scheduler
path identify this as a local job:

- `ic/src/history/store.rs:145-198`
- `ic/migrations/V5__claude_code.sql:1-2`

Recorded actions included:

```text
0  cd /home/dwarfstar/lunarwing/ic &&
   cargo test --test e2e_reflex_compiler 2>&1
   -> failed after 60,000 ms

1  cd /home/dwarfstar/lunarwing/ic &&
   cargo test --lib reflex::tests 2>&1
   -> failed after approximately 64,592 ms
```

Later actions repeatedly ran `ps` to look for `cargo test` and tailed
`/tmp/e2e_reflex_test.log`. This establishes that the agent observed builds
continuing after individual shell calls had already timed out.

The database has gaps in this job's action sequence (`2` through `5` were not
present). The command that created `/tmp/e2e_reflex_test.log` is therefore not
available in the surviving `job_actions` rows. This limits reconstruction of
the exact launch topology but does not alter the artifact evidence.

### 4. Worker cgroups were separate

The named workers ran as sibling user services, for example:

```text
lunarwing-opencode-dwarfstar.service
lunarwing-kimi-dwarfstar.service
```

During the second stop, systemd reported approximately:

- OpenCode: 499.9 MB peak.
- Kimi: 132.1 MB peak.

Those values cannot explain the main service's 37.6 GB peak and were not folded
into `lunarwing-dwarfstar.service` accounting.

### 5. No resource ceiling declared by the main tenant unit

The generated multi-tenant unit declares no `MemoryHigh=`, `MemoryMax=`,
`MemorySwapMax=`, `CPUQuota=`, `TasksMax=`, or comparable build-specific limit:

`ic/scripts/lunarwing-mt-admin.sh:6498-6523`

`NoNewPrivileges=true` and `PrivateTmp=true` are security controls, not resource
limits. The generated unit therefore supplied no tenant-specific ceiling;
inherited slice limits, service-manager defaults/drop-ins, system-wide
reclaim/swap policy, or OOM intervention could still bound it. The investigation
did not record every ancestor or manager-level control, so this report does not
claim that no host-level limit existed.

## Source-level failure chain

### Docker availability selects the local worker backend

At startup, Docker availability is probed. If Docker is missing or not running,
the code logs that the sandbox is disabled for the session and does not create
a container job manager:

`ic/src/orchestrator/mod.rs:87-135`

The incident journal contained:

```text
Docker is installed but not running -- sandbox disabled for this session.
```

For `create_job(mode="worker")`, the implementation uses a container only when
`sandbox_enabled()` is true. Otherwise it calls `execute_local(...)`, which
runs a separate in-process `Worker` through the scheduler:

- `ic/src/tools/builtin/job.rs:468-515,1436-1517`
- `ic/src/agent/scheduler.rs:119-237`

This is deliberate availability-dependent routing, not source evidence of a
sandbox escape. The dynamic tool description says “local backend” when external
workers are configured without a sandbox, and the no-sandbox schema describes a
local tracked job. Host shell availability is conditional: `app.rs` registers
development tools when local tools are enabled or when builder mode is enabled
and the sandbox is disabled. The persisted shell actions prove that condition
was met for this incident. The operational problem is that this local backend
then ran host shell commands in the main service cgroup without build-specific
limits:

- `ic/src/app.rs:474-485,941-947`
- `ic/src/tools/registry.rs:425-438`

### The host shell has no process-group boundary

The direct shell path creates `sh -c`, pipes stdout/stderr, and spawns it with
`tokio::process::Command`:

`ic/src/tools/builtin/shell.rs:658-701`

It does not configure:

- a new Unix process group;
- `kill_on_drop(true)`;
- a cgroup/scope;
- a descendant-process reaper.

It reads at most 64 KiB of raw bytes from each output stream before lossy UTF-8
conversion and drains the remainder to a sink (`shell.rs:703-742`). Conversion
and stream-combining can add overhead, but this path still cannot retain tens of
gigabytes of Cargo output.

### Timeout ownership is inconsistent

`ShellTool` internally defaults to 120 seconds:

`ic/src/tools/builtin/shell.rs:62-66`

But it does not override the generic `Tool::execution_timeout()`, whose default
is 60 seconds:

`ic/src/tools/tool.rs:343-348`

Both normal chat/Engine execution and the local job worker apply that generic
deadline around the entire tool future. The incident's `source=direct` local job
used the worker-specific wrapper:

- `ic/src/tools/execute.rs:63-67`
- `ic/src/worker/job.rs:611-617`

When the outer 60-second timeout fires, it drops the shell future before the
shell's internal 120-second timeout and cleanup branch necessarily run. Tokio's
process handle defaults do not guarantee that dropping the handle terminates
the OS child.

Even when the inner timeout branch is reached, it calls `child.kill()` only on
the direct `sh` child:

`ic/src/tools/builtin/shell.rs:758-768`

Without process-group ownership, that does not guarantee termination of Cargo,
`rustc`, linker, or backgrounded descendants.

Reference: [Tokio process `Child::kill` and `Command::kill_on_drop`](https://docs.rs/tokio/latest/tokio/process/index.html).

### Job cancellation aborts the Rust task, not necessarily the OS tree

The scheduler sends `WorkerMessage::Stop`, waits 100 ms, then aborts the Tokio
worker task if it remains active:

`ic/src/agent/scheduler.rs:586-635`

`stop_all()` applies this during daemon shutdown:

`ic/src/agent/scheduler.rs:719-732`

Aborting the Rust task is not equivalent to killing every subprocess it spawned.
The shell/process layer must own and reap the OS process group for cancellation
to be reliable.

## Why Engine output was not the primary cause

### Actual incident payloads were small

Read-only aggregate queries found:

- Job `775d1565`: 25 events totaling approximately 7.5 KB.
- Its persisted shell action output: approximately 4 KB raw in total.
- The other implicated job `aa429a6d`: 29 events totaling approximately 8.9 KB.
- All current Engine runtime documents: approximately 12.3 MB.
- Largest Engine thread document: approximately 488 KB.
- All conversation-message content: approximately 809 KB.
- Entire Dwarfstar PostgreSQL database at investigation time: approximately
  22 MB.

Those payloads cannot directly account for a 37.6 GB cgroup peak.

### Monty lifecycle

Source inspection found:

- A fresh outer Monty runner per Engine loop.
- A fresh nested runner per CodeAct block.
- No global pool retaining VMs, heaps, or `RunProgress` snapshots.
- Gate/restart checkpoints contain ordinary JSON state, not serialized VMs.
- Successful and failed calls unwind local VM ownership normally.

No source-level cross-call Monty VM leak was identified.

There are real host-side retention risks that warrant separate work:

- complete thread/event/conversation objects cached in `HybridStore`;
- startup-only terminal-state eviction;
- Python stdout collected in a host `String` with no explicit byte cap; Monty's
  duration/allocation/memory limits and the Engine iteration budget are only
  indirect bounds;
- spawned Tokio work that may continue when an unawaited `JoinHandle` is dropped;
- checkpoint/history duplication without an independent aggregate byte cap.

These are not dismissed. They simply do not match the direct compiler evidence
for this incident.

Key paths:

- `ic/src/bridge/store_adapter.rs:71-88,117-207`
- `ic/src/bridge/router.rs:1088-1103`
- `ic/crates/lunarwing_engine/src/executor/orchestrator.rs:111-120,327-638,1750-1780`
- `ic/crates/lunarwing_engine/src/executor/scripting.rs:49-55,274-379,423-756`
- `ic/crates/lunarwing_engine/orchestrator/default.py:299-369`

### Wasmtime lifecycle

Successful WASM calls create fresh stores and instances. Long-lived state holds
compiled engines/components by registered name, not one store per invocation.
The principal incident tools—`shell`, `read_file`, `create_job`, `job_status`,
and `job_events`—are native Rust tools.

No evidence ties a Wasmtime store leak to the 34.8/37.6 GB peaks.

Separate WASM defects were found and should be tracked independently:

- shutdown aborts only the stored polling supervisor handle; dropping its active
  inner polling `JoinHandle` detaches that task;
- running `spawn_blocking` callbacks can outlive outer async timeouts;
- channel epoch interruption is configured without an advancing epoch ticker;
- host allocations are outside guest linear-memory limits;
- chunked HTTP responses are capped after full buffering.

Key paths:

- `ic/src/tools/wasm/runtime.rs:153-232`
- `ic/src/channels/wasm/runtime.rs:117-179`
- `ic/src/tools/wasm/wrapper.rs:622-645,1204-1302,1473-1496`
- `ic/src/channels/wasm/wrapper.rs:455-476,1153-1229,2278-2562,2950-2968`
- `ic/src/tools/wasm/limits.rs:62-160`

## Commit `d2ff7b8`: valid but secondary

Before `d2ff7b8`, Engine V2 sanitized tool output and then attempted to parse the
original serialized JSON:

```rust
let sanitized = self.safety.sanitize_tool_output(lookup_name, &output);
let wrapped = self.safety.wrap_for_llm(lookup_name, &sanitized.content);
let output_value = serde_json::from_str::<serde_json::Value>(&output)
    .unwrap_or(serde_json::Value::String(wrapped));
```

Because normal tool output is valid JSON, parsing usually succeeded and the
sanitized/truncated/redacted value was discarded.

After `d2ff7b8`, the adapter drops the original serialized string and parses
`sanitized.content` instead:

`ic/src/bridge/effect_adapter.rs:775-786`

This makes Engine V2 consume the safety layer's transformed output instead of
the original serialization. Inputs already over the configured size limit are
truncated before they enter Engine/Monty state. Inputs at or below that limit
also receive leak redaction and policy enforcement, plus injection sanitization
when that check is enabled or policy-forced. Those later transformations are not
re-capped and can change the final length. Oversized output returns immediately
after truncation, so `d2ff7b8` does not by itself guarantee redaction of the
retained prefix. The new regression test passed:

```text
bridge::effect_adapter::tests::
engine_output_honors_safety_size_limit_before_monty_conversion ... ok
```

It remains a partial boundary fix:

- tools still construct and serialize full output before sanitization;
- `job_events` builds its aggregate before the safety cap;
- multiple bounded copies remain downstream;
- `d2ff7b8` performs no eager migration of persisted data. The default Engine
  orchestrator already lazily truncates historical `ActionResult` messages to
  8,000 characters before reuse, but source job/event rows remain unchanged
  (`ic/crates/lunarwing_engine/orchestrator/default.py:258-274,485-501`);
- aggregate history retention is not bounded by a per-result limit.

Chronologically, `d2ff7b8` was committed after the incident. It cannot be cited
as the root-cause fix for the observed compiler-driven peak.

## Findings by confidence

### Proven

- Systemd's reported values were unit-cgroup peaks, not recorded main-PID RSS.
- The second invocation lasted approximately 114 seconds, not 14 wall minutes.
- It accumulated approximately 14 CPU-minutes.
- Rust compilation modified 1,844 Dwarfstar target files whose aggregate final
  size was approximately 2.91 GB during that invocation.
- Job `775d1565` invoked Cargo tests and its shell calls timed out.
- Cargo processes were observed after earlier tool timeouts.
- `mode=worker` deliberately selects the local scheduler when no container job
  manager exists; its dynamic schema describes that configuration as local.
- Direct shell execution has no dedicated process group or cgroup.
- The generic 60-second timeout can preempt the shell's internal 120-second
  cleanup path.
- The generated main tenant unit declared no memory, swap, PID, or CPU ceiling;
  inherited ancestor-cgroup controls were not fully recorded.
- Actual Engine/job payload sizes were orders of magnitude too small to explain
  37.6 GB directly.
- `d2ff7b8` fixes a real output-safety bypass but not local-build isolation.

### High-confidence inference

Parallel Cargo/rustc/linker work, plus charged page cache and supporting kernel
memory, accounted for most of the 34.8/37.6 GB service-cgroup peaks.

This inference is supported jointly by the CPU-to-wall ratio, exact target-tree
timestamps, recorded Cargo commands, observed post-timeout Cargo activity,
normal post-build daemon footprint, and lack of a comparably sized persisted or
runtime data source.

### Remaining uncertainty

Historical PID-level and cgroup-member telemetry was not captured during either
peak. Therefore the following cannot be reconstructed with certainty:

1. The RSS/PSS contribution of each `rustc`, linker, Cargo, daemon, and page
   cache component at the exact peak.
2. The anonymous-versus-file split in `memory.stat` at the exact peak.
3. Whether one compiler tree survived the first service stop, was relaunched by
   an unpersisted action, or continued through another detached execution path.
4. The command represented by missing job action sequence numbers `2` through
   `5`, including the exact creator of `/tmp/e2e_reflex_test.log`.
5. Why no useful historical process-accounting records remain; the configured
   `/var/account/pacct` file is empty.

The current unit uses systemd's default `KillMode=control-group`, and its unit
file predates the incident. A normal service stop should clean the cgroup. The
second invocation's build activity therefore deserves an instrumented
reproduction rather than an unsupported assertion about exactly how it crossed
the restart boundary.

No heap profile was collected, so this report does not claim that LunarWing has
no other memory leaks. It concludes that this particular 34.8/37.6 GB incident
is explained by build resource use far better than by the previously proposed
Monty/Wasmtime leak.

## Recommended remediation

### P1: make the effective backend explicit

`mode=worker` is an availability-dependent compatibility value: it maps to the
Docker container manager when present and to the local scheduler otherwise. The
dynamic schema already distinguishes these cases, so local routing is not itself
a sandbox escape. Persisted `job_mode=worker` is also the database default for
direct jobs, so it does not reveal which backend actually ran and complicated
this investigation.

Recommended behavior:

- Record and expose an effective backend such as `local`, `docker`, or the named
  external worker in job status, events, and telemetry.
- Warn clearly when the effective backend is local and lacks container/cgroup
  isolation.
- Let deployments that require isolation reject local execution through an
  explicit policy; do not silently reinterpret a permitted local backend as
  sandboxed.
- Preserve named external workers (`opencode`, `kimi`, etc.) as separate,
  separately accounted backends.

### P0: own the complete subprocess tree

On Unix:

- Start every shell invocation in a dedicated process group.
- On timeout, cancellation, interrupt, or shutdown, signal the complete group.
- Escalate from graceful termination to `SIGKILL` after a bounded grace period.
- Reap the direct child and verify no group members remain.
- Use a cancellation guard so future/task drop also initiates cleanup.

Implement equivalent lifecycle guarantees on both systemd and OpenRC hosts.
Process-group cleanup belongs in the shell/process layer; init-specific cgroup
logic belongs in `lunarwing-mt-admin.sh` or the owning infrastructure layer.

### P0: unify timeout ownership

There should be one authoritative timeout for shell execution. The generic tool
wrapper must not expire before the shell's cleanup logic can run.

Possible approaches:

- Override `ShellTool::execution_timeout()` and use a shorter internal execution
  deadline plus cleanup allowance; or
- move timeout/cancellation ownership into a shared process supervisor that the
  generic wrapper invokes explicitly.

The invariant is more important than the exact shape: timeout completion must
mean the OS process tree is gone.

### P0: isolate builds with hard resource controls

Builds should run in a dedicated container or cgroup with explicit limits:

- memory high/hard limit;
- swap limit;
- PID/task limit;
- CPU quota or bounded Cargo parallelism;
- wall-clock deadline.

The repository's native build helper detects inherited cgroup constraints to
choose `cargo build -j`, but it does not create an isolation boundary and the
incident invoked `cargo test` directly rather than through that helper
(`scripts/build-lunarwing.sh:200-371,523-562`). Detection is not a substitute
for enforcement.

### P1: add a tenant safety ceiling

The main daemon unit should have a deliberately selected last-resort ceiling so
one tenant cannot consume the host. This is defense in depth, not a substitute
for per-build isolation.

Any systemd change must have an OpenRC-equivalent operational policy. Avoid
hardcoding assumptions that only work with system-bus systemd; tenant systemd
units run under the tenant's user manager.

### P1: add attribution telemetry

For every shell/build invocation, record non-sensitive metadata:

- invocation ID;
- PID and process-group ID;
- cgroup path;
- requested and effective timeout;
- start/end/cancel reason;
- whether escalation was required;
- maximum sampled child RSS and child count.

This would turn a future incident from inference into direct attribution.

### Separate follow-up work

Track these independently rather than combining them with BUG-005:

- periodic `HybridStore` terminal-state eviction;
- bounded Monty stdout collection;
- abort-on-drop wrappers for nested Engine tasks;
- WASM channel polling shutdown and epoch handling;
- streaming HTTP response caps;
- bounded external-worker accumulated progress;
- aggregate and pre-sanitization limits for `job_events` and other large tools;
- oversized-output redaction ordering in `SafetyLayer`.

## Reproduction and acceptance plan

Use a disposable tenant. Do not reproduce on a production tenant without a
hard memory limit.

### Measurements

Sample at least once per second:

- main PID `/proc/<pid>/status`: `VmRSS`, `VmHWM`, `VmSwap`, `VmData`;
- exact service cgroup: `memory.current`, `memory.peak`, `memory.swap.current`,
  `memory.stat`, `cpu.stat`, `pids.current`, `cgroup.procs`;
- all `cargo`, `rustc`, and linker processes: PID, PPID, PGID, RSS/PSS, command,
  and cgroup;
- container, OpenCode, and Kimi cgroups separately;
- system memory PSI and swap usage.

### Workload matrix

1. Idle/read-only baseline.
2. Sequential `read_file` workload.
3. `job_events` workload using bounded synthetic events.
4. One compile in an isolated worker.
5. Compile cancelled before completion.
6. Compile forced past its timeout.
7. Mixed read/shell/job workload approximating the original session.

### Acceptance criteria

- No Cargo, `rustc`, or linker process remains after timeout or cancellation.
- The compile is charged to a separate, bounded cgroup/container.
- Main LunarWing PID RSS remains near baseline.
- Main service `memory.current` returns near baseline after work completes.
- The build cannot drive system-wide swap pressure or OOM.
- Stopping/restarting the tenant leaves no build descendants.
- The Engine output-boundary regression from `d2ff7b8` remains green.
- Behavior is validated under both systemd-user and OpenRC tenant management.

## Verification record

The original investigation used source baseline `6c82e8c`; the report was added
in `11b673d`. Its recorded checks were:

- `taskset ... cargo test --lib engine_output_honors_safety_size_limit_before_monty_conversion`: passed.
- `taskset ... cargo check`: passed.
- `cargo fmt --all -- --check`: passed.
- Worktree was clean before this document was created.
- Long Rust commands were run detached in `tmux`.
- No live tenant was restarted or modified during the investigation.
- Database inspection was read-only and limited to metadata, counts, timestamps,
  action names, commands, and byte sizes needed for attribution.

Cross-verification against source baseline `1e319bf` on 2026-08-02 confirmed:

- among incident-relevant source paths, the only change since tenant revision
  `5b507966` was the Engine output-boundary fix `d2ff7b8`;
- Docker-unavailable `mode=worker` still deliberately routes through the local
  scheduler, and its dynamic schema describes the effective backend as local;
- direct shell execution still lacks process-group ownership and `kill_on_drop`;
- the generic tool deadline remains 60 seconds while the shell default remains
  120 seconds;
- scheduler cancellation still aborts the Tokio task after 100 ms without an OS
  process-tree cleanup contract;
- the generated main tenant unit still declares no memory, swap, PID, or CPU
  ceiling (inherited slice limits and manager defaults/drop-ins remain possible);
- `taskset -c 0-5 cargo test -j6 --lib bridge::effect_adapter::tests::engine_output_honors_safety_size_limit_before_monty_conversion -- --exact --nocapture --test-threads=6`
  passed (`1 passed; 0 failed`);
- the two exact `lunarwing_engine` output-history tests passed (`1 passed` each);
- `taskset -c 0-5 cargo test -j6 -p lunarwing_safety adversarial -- --nocapture --test-threads=6`
  passed (`98 passed; 0 failed`);
- `scripts/tests/test-build-lunarwing.sh` passed (`37 passed; 0 failed`).

No live tenant or historical incident data was accessed during this
cross-verification.

### Live rebuild verification on `8c6a1da` (2026-08-03)

Dwarfstar was upgraded from the incident-era checkout to
`ds6/dwarfstar/v2.0.4.0` at commit
`8c6a1da6113a636e7dc6af884248a2a191c7638c`. The rebuild and initial startup
were observed read-only with one-second samples written by a detached `tmux`
monitor.

Attribution results:

- The native release command was launched at `01:18:38-04:00` as
  `cargo build --profile release --bin lunarwing` and completed at
  `01:23:15-04:00`.
- The launcher's journal metadata placed it in the operator's
  `/user.slice/user-1000.slice/session-8.scope`, not
  `lunarwing-dwarfstar.service`.
- The tenant service was stopped during that build. Its cgroup did not exist in
  the sampler until startup at `01:37:47-04:00`.
- The native build command still specified no `-j`, `taskset`, `MemoryMax`, or
  dedicated transient scope. No `CARGO_BUILD_JOBS` or `BUILD_JOBS` override was
  present in the upgrade process environment, so Cargo could use host defaults.
- The native stage produced 449 release artifacts with an aggregate final size
  of about 947 MB. The final binary was 104,210,720 bytes. These are artifact
  sizes, not write-traffic or RSS measurements.
- The live sampler began after the native Cargo process exited, so it did not
  capture per-`rustc` RSS for that stage. The broad operator login scope is not
  usable as a build-memory measurement because it also contained Firefox,
  Kimi Code, and other desktop processes.
- Worker-image build steps were charged to separate Buildah/libpod scopes.
  Individual observed scope peaks included approximately 4.4-4.9 GB; the
  sampled upgrade-process tree peaked near 4.6 GiB RSS. None of this memory was
  charged to the stopped Dwarfstar daemon service.
- The daemon initially reached approximately 410 MiB, then rose to a stable
  plateau: about 440 MiB for the service cgroup and 448-451 MiB `VmRSS`/`VmHWM`
  for the main PID (`/proc` reports those fields in KiB). The cgroup carried
  approximately 388 MiB anonymous and 49 MiB file memory, zero swap, and no
  Cargo, `rustc`, or linker processes.
- The plateau was stable rather than monotonic growth. A five-minute sample
  window remained between approximately 438 and 446 MiB cgroup memory, with
  only about 3 MiB net anonymous growth.
- OpenCode, Kimi, PostgreSQL, and vision ran in sibling service cgroups. Their
  observed peaks were approximately 661 MiB, 208 MiB, 113 MiB, and 60 MiB,
  respectively.
- No Cargo, `rustc`, or linker process remained after the upgrade completed.
- The service was active and ready, and the tenant source and investigation
  checkout both resolved to `8c6a1da`.

The elevated daemon baseline was traced to startup high-water retention, not
live Engine state or compiler descendants:

- Dwarfstar created 32 normal Tokio runtime workers and, about 0.5 seconds later,
  another 32 `tokio-rt-worker` threads. A later source-level recheck found that
  thread names and timing do not prove the second group was Wasmtime-WASI's
  process-global ambient runtime. Tokio 1.53 enters the owning runtime handle on
  its blocking-pool threads, so metadata extraction reached through
  `tokio::task::spawn_blocking` should not take Wasmtime-WASI's no-runtime
  fallback. The second group could instead include Tokio blocking-pool workers,
  or an ambient runtime initialized from another unentered thread. Historical
  thread stacks were not captured, so exact ownership remains unknown.
- A current-source reproduction with eight tools created eight main runtime
  workers followed by eight same-runtime blocking-pool workers while loading
  WASM components. This strongly supports blocking-pool ownership for the
  historical second group, though it cannot retroactively prove thread identity
  without the missing Dwarfstar stacks.
- Startup loaded all installed `*.wasm` tool files concurrently. Dwarfstar had
  15 tool components, not the eight current install names. Seven pre-rename
  files (`gitlab.wasm`, `gotify.wasm`, `llm-context.wasm`,
  `multica-bridge.wasm`, `ssh.wasm`, `vision-analyze.wasm`, and
  `web-search.wasm`) survived from 2026-08-01 alongside the newly installed
  `*-tool.wasm` files. `install_wasm_tenant()` overwrites current artifacts but
  does not prune stale names; the loader therefore compiled and retained both
  sets by distinct names.
- `/proc/<pid>/smaps` showed 85 anonymous VMAs aligned to 64 MiB arena
  boundaries, carrying about 350 MiB anonymous RSS. A read-only `malloc_stats()`
  snapshot reported 86 glibc arenas with 463,802,368 bytes of allocator system
  memory but only 47,936,640 bytes in use. This explains most of the elevated
  RSS as freed startup allocations retained in per-thread arenas.
- Wasmtime-WASI 36.0.12 and 36.0.13 have the same ambient-runtime implementation,
  so the 36.0.13 security bump did not introduce that runtime design. Regardless
  of second-group ownership, the larger baseline is driven by startup fan-out,
  thread/arena count, and the stale component set rather than a per-call Wasmtime
  store leak.

A 2026-08-03 release-profile comparison used an isolated no-database process on
an eight-CPU host with the eight current WASM tool components:

- With eight async workers, sequential component loading, and Tokio's default
  blocking-pool limit, startup reached 140,900 KiB RSS with 20 threads. Sixteen
  threads carried the main runtime's explicit `lunarwing-tokio` name: eight
  configured async workers followed by eight lazily-created blocking workers.
  RSS remained 112,080 KiB after 13 minutes.
- Capping the blocking pool at four reduced `VmRSS` and `VmHWM` to 61,812 KiB.
  The value remained stable after 100 seconds with 12 threads: eight async
  workers, the main thread, the WASM epoch ticker, and two support threads.
- Main runtime defaults are now bounded at eight async workers and four blocking
  workers, with `LUNARWING_TOKIO_WORKER_THREADS` and
  `LUNARWING_TOKIO_MAX_BLOCKING_THREADS` overrides. Installed WASM tools and
  channels compile sequentially, and known stale pre-rename tool artifacts are
  ignored when their current `*-tool` replacement exists.

This live rebuild supports the central accounting conclusion: an operator-run
upgrade build is outside the tenant daemon cgroup, while a host-local shell
started by the daemon would inherit the daemon cgroup. It does **not** validate
shell timeout cleanup, because no in-daemon compile/cancel workload was run.

Source review on `8c6a1da` also found the incident's primary lifecycle risks:

- direct shell execution still has no process-group ownership or
  `kill_on_drop` guard;
- the generic tool timeout remains 60 seconds while the shell's internal default
  remains 120 seconds;
- `mode=worker` still routes to the local scheduler when the container manager
  is unavailable;
- `build_tenant()` still invokes bare Cargo without explicit parallelism or a
  dedicated bounded build scope;
- generated tenant units still declare no explicit memory, swap, PID, or CPU
  ceiling.

## Research provenance

Shared study-note topics `engine-v2`, `bridge`, `wasm-tool-sandbox`, and
`tools-infrastructure` were used as an initial map. Their source-note paths
include:

- `aj-codebase-study/engine-v2.md`
- `rares-codebase-study/engine-v2.md`
- `sb-codebase-study/engine-v2.md`
- `aj-codebase-study/bridge.md`
- `aj-codebase-study/wasm-tool-sandbox.md`
- `aj-codebase-study/tools-infrastructure.md`

The 2026-08-02 cross-verification also consulted shared topic
`tools-builtin-impls` (`aj-codebase-study/tools-builtin-impls.md` and
`rares-codebase-study/tools-builtin-impls.md`) as an initial map. All current
behavioral claims were then checked directly against source and repository
history. Historical host, journal, database, and artifact claims retain the
original investigation's provenance and were not independently reproduced.

## Final classification

**Primary incident:** a resource-unbounded local Rust build plus process-tree
lifecycle failure. Systemd cgroup accounting correctly exposed the aggregate
resource use.

**Not established:** a sandbox escape, Monty VM leak, Wasmtime per-call store
leak, or 37.6 GB LunarWing main-process RSS.

**Valid secondary fix:** `d2ff7b8` feeds the safety layer's transformed output
into Engine V2 after the tool has produced its full result. Oversized inputs are
truncated; normal-sized inputs receive the later redaction/policy stages, whose
output is not re-capped; oversized prefixes currently receive no redaction.

The release checklist should not mark the broad “MEMLEAK” item complete solely
because `d2ff7b8` landed. The output-boundary defect is fixed; process-group
cancellation and per-build resource controls remain open. Effective-backend
attribution and an optional fail-closed deployment policy remain recommended
hardening.

## 2026-08-03 postscript — cgroup isolation fix (branch `ds6/dwarfstar/v2.0.4.0`)

The two open items above (process-group cancellation, per-build resource
controls) are addressed in this branch:

- **Portable tree reaping (all init systems).** New module
  `ic/src/tools/process.rs`: every tool-spawned workload now leads its own
  process group with `kill_on_drop`, and a `ProcessTreeGuard` SIGTERM/SIGKILLs
  the whole group on timeout, cancellation, or drop of the execute future.
  `ShellTool` now overrides `execution_timeout()` (internal 120 s + 5 s grace)
  so its own reaping path runs before the generic outer timeout drops the
  future — the failure mode that orphaned `cargo`/`rustc` during the incident.
  Wired into `ic/src/tools/builtin/shell.rs`,
  `ic/src/sandbox/manager.rs::execute_direct`, and the `cargo component build`
  path in `ic/src/registry/artifacts.rs` (which also gained a 20-minute bound).
- **systemd transient scopes.** On hosts with a reachable user manager
  (auto-probed once, cached, graceful fallback), workloads spawn via
  `systemd-run --user --scope --quiet --collect` under
  `lunarwing-workloads.slice`, so build memory/CPU is accounted and capped
  outside `lunarwing-<tenant>.service`. Env knobs:
  `LUNARWING_WORKLOAD_ISOLATION=auto|none`, `LUNARWING_WORKLOAD_SLICE`,
  `LUNARWING_WORKLOAD_MEMORY_MAX`.
- **mt-admin deployment caps.** `render_tenant_systemd_units()` now renders
  `lunarwing-workloads.slice` (default 6G/8G, CPUQuota 800%, TasksMax 2048)
  and `lunarwing-build.slice` (16G/24G), and the daemon unit itself gets
  `MemoryHigh=1536M MemoryMax=2G TasksMax=512` (observed daemon plateau
  ~450 MiB). `build_tenant()` runs cargo inside the build slice with an
  explicit `-j` (default `nproc`). All values are `LUNARWING_MT_*`
  overridable. OpenRC units gain a documented-gap comment; the portable
  tree-reaping layer still applies there.

Validation: `cargo check` clean; new `tools::process` unit tests cover
group kill on timeout, on future drop, and disarm safety; mt-admin render
harness `ic/scripts/tests/test-workload-isolation-rendering.sh` (19 checks)
covers slice rendering, daemon caps, overrides, OpenRC parity, and scoped vs
nice'd build paths; shellcheck and full `cargo clippy -D warnings` clean.

Live validation on Dwarfstar (2026-08-03, commit `238b281` + follow-up):
- `upgrade-tenant dwarfstar --target ds6/dwarfstar/v2.0.4.0` completed
  cleanly (verified DB backup, stop, checkout, rebuild, re-render, start).
- Rendered and loaded: `lunarwing-workloads.slice` (6G/8G, CPUQuota 800%,
  TasksMax 2048), `lunarwing-build.slice` (16G/24G), daemon unit
  `MemoryHigh=1536M MemoryMax=2G TasksMax=512`.
- `systemd-run --user --scope` probe succeeds for the tenant user;
  `_run_tenant_build` places commands in
  `user@1008.service/lunarwing.slice/lunarwing-build.slice/run-*.scope`
  (verified via `/proc/self/cgroup` from inside a scoped command).
- One bug was found and fixed during validation: `_run_tenant_build`
  initially consumed `ensure_init_system` as if it printed the value, but it
  only sets the `$INIT_SYSTEM` global, so builds silently took the OpenRC
  fallback. Fixed; harness stubs updated to match real semantics.
- First agent-driven shell call on the daemon confirmed in the user journal:
  the one-time probe (`/usr/bin/true` scope) fired, then the agent's `sh -c`
  command ran as `run-*.scope` under `lunarwing-workloads.slice` while the
  daemon cgroup stayed flat and the scope was collected afterward.

## 2026-08-03 postscript 2 — baseline fix merged and verified live

The idle-baseline fix from `g6/g/v2.0.4.0` (`cd6a16a`: bounded Tokio worker
and blocking pools, sequential WASM compilation, legacy-artifact shadowing)
was merged into `ds6/dwarfstar/v2.0.4.0` as `cfa7c52`. Dwarfstar was rebuilt
with `upgrade-tenant` on the merge commit. Measured after startup settled:

- daemon main PID: **91.7 MB `VmRSS`/`VmHWM`, 12 threads** (previously
  ~461 MB / ~70 threads) — flat over repeated samples;
- daemon cgroup: **138 MB current / 142 MB peak**, of which only ~44 MB is
  anonymous (previously ~390 MB anonymous retained in glibc arenas); the rest
  is reclaimable file cache;
- the tenant build ran inside `lunarwing-build.slice`, which accounted ~4 GB
  peak under the 16G/24G caps instead of the operator session;
- zero workload-isolation warnings in the daemon journal; agent status
  endpoint healthy.

The ~450 MiB idle plateau and the 37.6 GB build-accounting incident are both
closed: baseline is back inside the historical 80–165 MB per-instance band,
and agent/operator compiles are capped and accounted outside the daemon
cgroup.

## 2026-08-14 postscript 3 — OpenRC hard ceilings (work item #54)

The remaining documented gap — "OpenRC has portable process-tree cleanup but
not systemd-equivalent hard memory, CPU, and task ceilings" — is now closed
at the service-management layer using OpenRC's native cgroup v2 support
(verified against OpenRC 0.63.1's `rc-cgroup.sh`):

- **Tenant daemon units** rendered by `render_tenant_openrc_units()` now
  compose `rc_cgroup_settings` (`memory.high`/`memory.max`/`cpu.max`/
  `pids.max`) and set `rc_cgroup_cleanup="YES"` (systemd
  `KillMode=control-group` parity: every leftover cgroup process is killed on
  stop). Because OpenRC has no transient scopes, the daemon and its
  agent-spawned workloads share one service cgroup, so the defaults are the
  workload outer bounds (6G/8G, 800% CPU as `800000 100000`, 2048 pids) rather
  than the smaller daemon-only caps. Per-tenant conf.d knobs
  (`lunarwing_cgroup_memory_max`, `..._memory_high`, `..._cpu_max`,
  `..._pids_max`, plus a `lunarwing_cgroup_limits_enabled` opt-out) tune or
  disable the ceilings; they apply only on cgroup v2 unified hosts, and OpenRC
  silently skips any knob whose controller file is not delegated, so the unit
  still starts on legacy hosts.
- **Operator builds** on non-systemd hosts (`_run_tenant_build`) now run
  inside a temporary, tenant-delegated cgroup v2 directory carrying the
  `BUILD_*` memory ceilings (16G/24G) — the portable equivalent of
  `lunarwing-build.slice` — with `cgroup.kill`/`cgroup.procs` reaping on
  completion. Setup failure falls back to the previous nice-only invocation,
  so a missing cgroup delegation never breaks a build; a build's own exit
  status is propagated unchanged.
- The single-node committed template `ic/systemd/lunarwing.confd` documents
  the same `rc_cgroup_settings` knobs as a commented, opt-in example,
  mirroring the single-node systemd template (which also ships without
  ceilings).

Render-harness coverage in
`ic/scripts/tests/test-workload-isolation-rendering.sh` (now 35 checks)
asserts the rendered ceilings, conf.d override propagation, the cgroup build
path preference, the setup-failure fallback, and the CPUQuota→cpu.max
conversion. Runtime behaviour on an actual OpenRC host remains to be
validated on the next Gentoo tenant reprovision; the dev VM is systemd-booted.
