## Goal 3 review findings and OpenRC validation

Review target: `rarity/kimi-3-20260814-0000` at `1d21122d`, merged into this
branch by `f9be5b0f`.

### Verified implementation

- Multi-tenant OpenRC units render `rc_cgroup_settings` for `memory.high`,
  `memory.max`, `cpu.max`, and `pids.max`, using default combined
  daemon/workload limits of 6 GiB, 8 GiB, 800%, and 2048 processes.
- Non-systemd operator builds use temporary `lunarwing-build-<tenant>` cgroup
  v2 directories with the 16 GiB/24 GiB `BUILD_*` memory thresholds.
- `ic/scripts/tests/test-workload-isolation-rendering.sh` passes all 35 checks.
  MR !469 pipeline 1740 passed all 17 jobs, but its OpenRC checks use rendered
  files and mocks rather than a booted OpenRC host.

### Findings to fix

1. **High — cleanup is broken on stock OpenRC 0.63.1.** The generated unit
   sets `rc_cgroup_cleanup="YES"` (`ic/scripts/lunarwing-mt-admin.sh:9395`), but
   OpenRC 0.63.1's `cgroup_running()` checks
   unprefixed cgroup v2 paths while `cgroup2_set_limits()` creates
   `openrc.${RC_SVCNAME}`. Cleanup therefore returns without killing leftover
   processes. OpenRC issue
   [#910](https://github.com/OpenRC/openrc/issues/910) documents the defect;
   upstream commit
   [`1cf3944`](https://github.com/OpenRC/openrc/commit/1cf3944ed9bc3843b98c0849d1023dc34c7235ad)
   fixes it. Check whether the Gentoo package backports that commit before
   claiming `KillMode=control-group` parity.
2. **High — build exit status 2 causes a second, unbounded run.**
   `_run_tenant_build_cgroup()` uses status 2 for setup failure but also
   propagates a real build status 2. `_run_tenant_build()` interprets either as
   setup failure and reruns the same command through the nice-only fallback.
   This occurs at `ic/scripts/lunarwing-mt-admin.sh:2540-2542,2571-2577,2603-2610`
   and was reproduced with function stubs: both bounded and fallback paths ran.
3. **High — build memory limits can silently be absent.** The availability
   probe checks only that the cgroup v2 root is writable. Missing controller
   files are skipped and failed `memory.high`/`memory.max` writes are ignored,
   so a build can be reported as bounded while running without hard limits
   (`ic/scripts/lunarwing-mt-admin.sh:2508-2513,2553-2564`).
4. **Medium — per-tenant limit overrides do not survive rendering.** The
   generated `/etc/conf.d/lunarwing-<tenant>` advertises editable cgroup knobs,
   but `render-units` truncates the file. Normal `upgrade-tenant` runs
   `render-units`, so operator overrides are erased
   (`ic/scripts/lunarwing-mt-admin.sh:9748-9767,11592-11594`).
5. **Low — effective enforcement lacks coverage.** Existing tests assert
   rendered strings and mocked path selection. They do not verify controller
   delegation, effective values, process placement, limit enforcement,
   interrupted-build cleanup, or service-stop cleanup on OpenRC
   (`ic/scripts/tests/test-workload-isolation-rendering.sh:91-142`).

### Fix status on this branch

The four high/medium findings above are fixed:

- Generated tenant init scripts wrap normal stop with bounded LunarWing-owned
  cgroup v2 cleanup, so cleanup still runs when `default_stop` fails. It checks
  both OpenRC path shapes, uses `cgroup.kill` after moving the OpenRC runner to
  the parent, falls back to bounded PID reaping, and makes `stop-tenant` fail
  when cleanup fails. This covers packages without upstream commit `1cf3944`.
- OpenRC memory values now accept byte counts or K/M/G/T binary suffixes and are
  converted to bytes before direct kernel cgroup writes. The previous `6G`,
  `8G`, `16G`, and `24G` writes were systemd syntax, not valid values for
  `memory.high` or `memory.max`.
- On cgroup v2 hosts, `start_pre` verifies effective memory, CPU, and pids files
  against configured values. Missing delegation or ignored OpenRC writes now
  fail daemon startup; `lunarwing_cgroup_limits_enabled=false` remains the
  explicit emergency opt-out. Legacy/cgroup-less hosts retain portable process
  cleanup without cgroup ceilings.
- Operator-build setup now requires the memory controller, enables and verifies
  subtree delegation, requires both memory files, verifies writes by readback,
  and uses an explicit setup-complete state. Every real build status, including
  2 and the post-setup placement status 125, is returned without a second run.
- `/etc/conf.d/lunarwing-<tenant>` now has one atomic managed dependency block.
  Rerenders update that block while preserving cgroup overrides, `rc_ulimit`,
  comments, unrelated settings, owner, and mode. Legacy generated files migrate
  on first render; symlinks and malformed/duplicate blocks fail safely.
- The focused harness now executes generated memory conversion and service
  cleanup helpers against inert cgroup fixtures, plus regression coverage for
  strict build setup, exit-status propagation, legacy conf.d migration,
  override preservation, malformed-file rejection, and rerender idempotence.

Disposable-tenant runtime validation below remains required before goal 3 can
be checked. Current development host runs OpenRC 0.63.1 without the upstream
`cgroup_running()` path fix, making the LunarWing fallback the required target
for disposable-tenant validation.

### Required OpenRC host validation

Use a disposable tenant. `restart-tenant` and `stop-tenant` below interrupt that
tenant. Run from the repository root after deploying this branch.

#### 1. Confirm cgroup v2 and OpenRC cleanup support

```bash
tenant=YOUR_DISPOSABLE_TENANT
admin=ic/scripts/lunarwing-mt-admin.sh
svc="lunarwing-$tenant"

test "$(stat -fc %T /sys/fs/cgroup)" = cgroup2fs
openrc --version
cat /sys/fs/cgroup/cgroup.controllers
cat /sys/fs/cgroup/cgroup.subtree_control

rc_cgroup_file=
for candidate in \
  /lib/rc/sh/rc-cgroup.sh \
  /usr/lib/rc/sh/rc-cgroup.sh \
  /usr/libexec/rc/sh/rc-cgroup.sh
do
  if [ -f "$candidate" ]; then
    rc_cgroup_file="$candidate"
    break
  fi
done
test -n "$rc_cgroup_file" || { printf 'rc-cgroup.sh not found\n'; exit 1; }
grep -nA6 '^cgroup_running()' "$rc_cgroup_file"
```

If `cgroup_running()` does not check `openrc.${RC_SVCNAME}` paths, OpenRC issue
#910 remains present. That is acceptable only when the rendered
`/etc/init.d/lunarwing-$tenant` contains
`_lunarwing_service_cgroup_cleanup`, `cgroup.kill`, and the bounded
`cgroup.procs` fallback. Section 3 must then prove that fallback works.

#### 2. Render, restart, and inspect effective service limits

```bash
sudo "$admin" render-units "$tenant"
sudo "$admin" restart-tenant "$tenant"
sudo "$admin" status "$tenant"

cg=
for candidate in \
  "/sys/fs/cgroup/openrc.$svc" \
  "/sys/fs/cgroup/unified/openrc.$svc"
do
  if [ -d "$candidate" ]; then
    cg="$candidate"
    break
  fi
done
test -n "$cg" || { printf 'service cgroup not found\n'; exit 1; }

for knob in memory.high memory.max cpu.max pids.max; do
  test -f "$cg/$knob" || { printf 'missing %s\n' "$knob"; exit 1; }
  printf '%s: ' "$knob"
  sudo cat "$cg/$knob"
done

while read -r pid; do
  test -r "/proc/$pid/cgroup" || continue
  sudo ps -o pid,ppid,user,comm,args -p "$pid"
  cat "/proc/$pid/cgroup"
done < "$cg/cgroup.procs"
```

With defaults and no parent cap, effective values should read as
`6442450944`, `8589934592`, `800000 100000`, and `2048`. At least the LunarWing
daemon must appear in `cgroup.procs` and report the same cgroup path through
`/proc/<pid>/cgroup`.

#### 3. Verify stop cleanup with a controlled leftover process

```bash
test_pid="$(
  sudo sh -c '
    sleep 600 >/dev/null 2>&1 &
    pid=$!
    printf "%s\n" "$pid" > "$1/cgroup.procs"
    printf "%s\n" "$pid"
  ' _ "$cg"
)"
grep -qx "$test_pid" "$cg/cgroup.procs"

sudo "$admin" stop-tenant "$tenant"
if sudo kill -0 "$test_pid" 2>/dev/null; then
  printf 'FAIL: leftover process %s survived service stop\n' "$test_pid"
  sudo kill -KILL "$test_pid"
else
  printf 'PASS: service stop reaped leftover process %s\n' "$test_pid"
fi
sudo "$admin" start-tenant "$tenant"
```

Do not accept goal 3 if the controlled process survives. Either OpenRC's
upstream commit `1cf3944` or LunarWing's rendered portable fallback must reap it.

#### 4. Inspect temporary operator-build memory limits

Run the OpenRC build-bounding primitive with a controlled 60-second command.
Use tmux because the probe is long-running:

```bash
tmux new-session -d -s "lw-build-cgroup-$tenant" \
  "sudo env LUNARWING_SERVICE_MANAGER=openrc bash -c 'source \"$admin\"; _run_tenant_build \"$tenant\" \"sleep 60\"'"

bcg="/sys/fs/cgroup/lunarwing-build-$tenant"
until [ -d "$bcg" ] || ! tmux has-session -t "lw-build-cgroup-$tenant" 2>/dev/null; do
  sleep 1
done
test -d "$bcg" || { printf 'temporary build cgroup not observed\n'; exit 1; }

for knob in memory.high memory.max; do
  test -f "$bcg/$knob" || { printf 'missing build %s\n' "$knob"; exit 1; }
  printf '%s: ' "$knob"
  sudo cat "$bcg/$knob"
done
sudo cat "$bcg/cgroup.procs"
```

Default effective values should be `17179869184` and `25769803776`, and
`cgroup.procs` must be non-empty while the build runs. Missing files, `max`, an
empty process list, or ignored write errors mean the build is not bounded.

### Goal 3 acceptance gate

Check goal 3 only after all high/medium findings are fixed, focused regression
tests cover exit-status propagation and conf.d preservation, installed OpenRC
contains the cleanup path fix (or LunarWing supplies its own fallback), and all
four host-validation sections pass.
