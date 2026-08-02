# BUG-005: Dwarfstar memory incident investigation

> **Status:** RECLASSIFIED — build-process isolation and cancellation failure
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
> **Cross-verification:** Rechecked against repository HEAD `1e319bf` on
> 2026-08-02. The source-level failure chain and open remediation status remain
> current. Historical tenant journal, database, filesystem, and `/proc` figures
> below are preserved investigation evidence; they cannot be independently
> reproduced from this checkout alone.
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

The incident is therefore best classified as:

> A Rust build escaped its intended worker isolation, ran under or was charged
> to the LunarWing service cgroup without a memory ceiling, and was not reliably
> terminated when the owning tool/job timed out or was cancelled.

Cgroups did not leak memory. They correctly accounted for the aggregate memory
used by the daemon and its charged descendants, including compiler memory,
file/page cache, and kernel allocations. The product defect is the placement,
resource isolation, timeout ownership, and process-tree cleanup around local
shell work.

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

### 5. No resource ceiling on the main tenant unit

The generated multi-tenant unit contains no `MemoryHigh=`, `MemoryMax=`,
`MemorySwapMax=`, `CPUQuota=`, or comparable build-specific limit:

`ic/scripts/lunarwing-mt-admin.sh:6498-6523`

`NoNewPrivileges=true` and `PrivateTmp=true` are security controls, not resource
limits. The service was therefore free to consume host memory until global
reclaim, swap pressure, or OOM intervention.

## Source-level failure chain

### Sandbox detection permits local fallback

At startup, Docker availability is probed. If Docker is missing or not running,
the code logs that the sandbox is disabled for the session and does not create
a container job manager:

`ic/src/orchestrator/mod.rs:87-135`

The incident journal contained:

```text
Docker is installed but not running -- sandbox disabled for this session.
```

For `create_job(mode="worker")`, the implementation uses the container only
when `sandbox_enabled()` is true. Otherwise it calls `execute_local(...)`:

`ic/src/tools/builtin/job.rs:1436-1517`

Thus a mode named `worker`, normally expected to provide container isolation,
can silently become an in-process local worker when the sandbox is unavailable.

### The host shell has no process-group boundary

The direct shell path creates `sh -c`, pipes stdout/stderr, and spawns it with
`tokio::process::Command`:

`ic/src/tools/builtin/shell.rs:658-701`

It does not configure:

- a new Unix process group;
- `kill_on_drop(true)`;
- a cgroup/scope;
- a descendant-process reaper.

Its stdout and stderr retention is bounded to 64 KiB per stream and remaining
output is drained to a sink (`shell.rs:703-742`). Therefore verbose Cargo output
was not the source of tens of gigabytes of retained strings.

### Timeout ownership is inconsistent

`ShellTool` internally defaults to 120 seconds:

`ic/src/tools/builtin/shell.rs:62-66`

But it does not override the generic `Tool::execution_timeout()`, whose default
is 60 seconds:

`ic/src/tools/tool.rs:343-348`

The canonical execution wrapper applies that generic timeout around the entire
tool future:

`ic/src/tools/execute.rs:63-67`

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
- unbounded collected Python stdout outside Monty's heap limit;
- detached Tokio tasks when pending handles are dropped;
- checkpoint/history duplication.

These are not dismissed. They simply do not match the direct compiler evidence
for this incident.

Key paths:

- `ic/src/bridge/store_adapter.rs:71-88,117-207`
- `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`
- `ic/crates/lunarwing_engine/src/executor/scripting.rs`

### Wasmtime lifecycle

Successful WASM calls create fresh stores and instances. Long-lived state holds
compiled engines/components by registered name, not one store per invocation.
The principal incident tools—`shell`, `read_file`, `create_job`, `job_status`,
and `job_events`—are native Rust tools.

No evidence ties a Wasmtime store leak to the 34.8/37.6 GB peaks.

Separate WASM defects were found and should be tracked independently:

- a channel polling child task can detach during shutdown;
- running `spawn_blocking` callbacks can outlive outer async timeouts;
- channel epoch interruption is configured without an advancing epoch ticker;
- host allocations are outside guest linear-memory limits;
- chunked HTTP responses are capped after full buffering.

Key paths:

- `ic/src/tools/wasm/wrapper.rs`
- `ic/src/channels/wasm/wrapper.rs:2278-2562,2950-2968`
- `ic/src/tools/wasm/limits.rs`

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

This correctly bounds newly executed outputs before they enter Engine/Monty
state and restores redaction/policy modifications for normal-sized output. The
new regression test passed:

```text
bridge::effect_adapter::tests::
engine_output_honors_safety_size_limit_before_monty_conversion ... ok
```

It remains a partial boundary fix:

- tools still construct and serialize full output before sanitization;
- `job_events` builds its aggregate before the safety cap;
- multiple bounded copies remain downstream;
- historical persisted output is not migrated;
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
- `mode=worker` selects local execution when the sandbox manager is absent.
- Direct shell execution has no dedicated process group or cgroup.
- The generic 60-second timeout can preempt the shell's internal 120-second
  cleanup path.
- The main tenant unit had no memory or swap ceiling.
- Actual Engine/job payload sizes were orders of magnitude too small to explain
  37.6 GB directly.
- `d2ff7b8` fixes a real output-safety bypass but not build isolation.

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

### P0: fail closed for worker isolation

`mode=worker` should not silently become in-process local execution when its
sandbox is unavailable.

Recommended behavior:

- Return an explicit “worker sandbox unavailable” error.
- If local execution remains supported, expose it as a distinct, explicit mode
  such as `mode=local` with an approval boundary and clear warning.
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

The existing native build helper detects cgroup constraints to choose Cargo job
count, but it does not create an isolation boundary. Detection is not a
substitute for enforcement.

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

On investigation branch `6c82e8c`:

- `taskset ... cargo test --lib engine_output_honors_safety_size_limit_before_monty_conversion`: passed.
- `taskset ... cargo check`: passed.
- `cargo fmt --all -- --check`: passed.
- Worktree was clean before this document was created.
- Long Rust commands were run detached in `tmux`.
- No live tenant was restarted or modified during the investigation.
- Database inspection was read-only and limited to metadata, counts, timestamps,
  action names, commands, and byte sizes needed for attribution.

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

All material claims in this report were rechecked against current source,
current host state, journal records, persisted metadata, or build-artifact
timestamps. The study notes are orientation, not authority.

## Final classification

**Primary incident:** build resource isolation/accounting and process-tree
lifecycle failure.

**Not established:** a Monty VM leak, Wasmtime per-call store leak, or 37.6 GB
LunarWing main-process RSS.

**Valid secondary fix:** `d2ff7b8` bounds and sanitizes newly executed Engine V2
tool results after the tool has produced them.

The release checklist should not mark the broad “MEMLEAK” item complete solely
because `d2ff7b8` landed. The output-boundary defect is fixed; the P0 worker
fallback, process-group cancellation, and build resource-isolation defects
remain to be implemented and verified.
