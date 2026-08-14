# PRE-RELEASE CHECKLIST for LunarWing v2.0.8.0

## Codename: Unknown

**Open TODOs (v2.0.7.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)

### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] update kimi dev loop if havent already - Rares
2. [x] update kimi dev loop if havent already - AJ
3. [ ] work on the following work item - https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/54 — PARTIAL 2026-08-14: OpenRC service cgroup v2 ceilings and non-systemd build memory cgroups are implemented, the 35-check render harness passes, and MR !469's 17-job pipeline passed. Review found correctness gaps and no real OpenRC runtime coverage; keep unchecked until fixes and the host validation below pass.
4. [x] verify that the following work item was completed - https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/50 verify if done — DONE (verified 2026-08-14 on aj/kimi-4-20260814-0001): fix cf4b1164 excludes `ThreadType::Mission` from the conversation-insights `thread_goals` payload + event listener skips Mission threads; e999db6d sorts goals by recency; regression tests `conversation_insight_goals_exclude_mission_threads` and `conversation_insight_goals_use_recency_not_store_order` pass (ic/crates/lunarwing_engine/src/runtime/mission.rs). Independent review found no correctness gaps; all 67 mission tests and MR !468 pipeline 1739 passed. Residual gap: no single integration test asserts the exact generated conversation-insights payload end to end.
5. [ ] verify that the following work item was completed - https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/47 verify if done
6. [ ] investigate if any crates should be upgraded for security reasons. make a new document in docs/ops detailing findings - you may use existing documentation for reference as well as the following work item https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/53 
7. [ ] clean up repo - remove uncessary files - organize files in repo root a bit better - for example - crszsslw2412c09c1-8ea9-46bd-8063-084ccdd2f332.jpg in root of repo - maybe change file name? and move very old documentation to historical archive directory. dont just delete it
8. [ ] implement Publisher signatures dependency locks, marketplace revocation distribution, mandatory sandbox profiles, persistent external-worker placement, a bundled gateway control-plane service, and generic external runtime descriptors for MCP part 2
9. [ ] refactor mt admin - you may use this old branch from 2.0.5.0 era for inspiration - in fact, most of this can be reused likely - ref2/sunburst/mtadmin-refactor-v2.0.6.0
10. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.7. we need bump to 2.0.8 - ensure CI/CD is also updated for the new crate versions — lockfiles must be refreshed - all channel registry patch versions must be also be bumped to comply with CI/CD by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; verify GitLab config validation passes if possible - cargo update and all that good stuff as usual
11. [ ] mcp part 3 work - see existing documentation for details - pick one thing from extensive list and update this goal with what was done. state any work that was deferred or is incomplete. ensure that the whole project builds after you finish your thing - see roadmap for information as well
12. [ ] Additional Opencode External Worker Polishing, including updating the version of opencode to 1.18.18
13. [ ] health service improvements - see recent documents in docs/proposals for more information
14. [ ] Cross-tenant knowledge sharing via coordination pipeline - see existing documentation
15. [ ] Vector embeddings hybrid ranking - see existing documentation
16. [ ] update roadmap in docs/ops with state of each goal
17. [x] (S) fix cicd again - RESERVED
18. [ ] mcp part 3 work - see existing documentation for details - pick one thing from extensive list and update this goal with what was done. state any work that was deferred or is incomplete. ensure that the whole project builds after you finish your thing - see roadmap for information as well - this is NOT a duplicate
19. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.7.0
20. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.8.0 explaining all relevant changes since v2.0.7.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. Keep it brief. The codename for this release is not know yet - The file you write will be `RELEASE-v2.0.8.0.md` and should be written to the ROOT of the repo
21. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
22. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
23. [ ] update anything outdated in docs/proposals - verify against code rather than docs
24. [ ] update anything outdated in docs/guides - verify against code rather than docs
25. [ ] update anything outdated in docs/internal - verify against code rather than docs
26. [ ] create a fourth document in docs/ops similar in nature to docs/ops/guide-for-spin-up-gentoo-tenants.md and the arch guide and the fedora guide but for debian/ubuntu GNU/Linux hosts
27. [ ] add new health stuff to roadmap and add new ideas discussed to roadmap that are not currently present
28. [ ] update release notes draft in root of repo with anything might have missed that may be relevant
29. [ ] update README.md in repo root (include information related to sandboxed WASM MCPs and any other relevant information that may be missing)

---

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
for candidate in /lib/rc/sh/rc-cgroup.sh /usr/lib/rc/sh/rc-cgroup.sh; do
  if [ -f "$candidate" ]; then
    rc_cgroup_file="$candidate"
    break
  fi
done
test -n "$rc_cgroup_file" || { printf 'rc-cgroup.sh not found\n'; exit 1; }
grep -nA6 '^cgroup_running()' "$rc_cgroup_file"
```

`cgroup_running()` must check `openrc.${RC_SVCNAME}` paths. If it does not,
OpenRC issue #910 remains present and cleanup validation will fail.

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

Do not accept goal 3 if the controlled process survives. Apply or require an
OpenRC package carrying upstream commit `1cf3944`, or implement a LunarWing-side
portable cleanup fallback.

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
