# Release Notes for LunarWing v2.2.1.0

## Codename: Shinobi (忍び) — The One Who Endures

**Release Date:** 2026-11-20

## Highlights

- **The crate baseline moves to `2.2.1` (WI#767).** The daemon, its five
  workspace crates, the XMPP bridge and the four WASM channels are at
  `2.2.1`; every registry entry is bumped with them, and the vendor tree and
  lockfiles were regenerated together. The security audit that follows the
  sweep (WI#784) reads 27 tracked non-vendor lock roots, 4,200 package
  records and 4,126 registry pins at `4192ee72b`.
- **The advisory baseline is 23 acceptances (WI#781, WI#787, WI#788,
  WI#784).** The two Wasmtime IDs accepted on 2026-11-19 are gone: RustSec
  corrected both records to `unaffected` for the locked 36.0.x line, so
  `cargo-deny` reports them as not detected and WI#787 removed them. Nine
  `heapless` unsoundness advisories are accepted for one graph position only,
  and `[bans]` entries now pin both `heapless <0.9.4` and its `postcard`
  parent to that position.
- **The sandbox relay now labels its listener for SELinux (WI#641).** On a
  host with SELinux enabled the relay socket is created under a
  container-shaped context and the daemon replays the container's
  `connect` under the host's own policy before any container starts. A
  refused candidate falls through to the next, and a host whose policy refuses
  all of them still binds the unlabeled socket it bound before. No policy is
  changed and no sandbox is made unconfined. The change is unit-tested; no run
  on an enforcing host is recorded.
- **The gated tunnel suite runs on Docker as well as Podman (WI#782).** The
  lane is opened by one runtime gate and refuses both at once, and the test
  functions no longer carry a runtime name. The Docker fallback was exercised
  live for the first time, and the detached-unit checks state their skip where
  no unit outlives a `docker run -d` client.
- **`stop-tenant` no longer leaks the egress keeper when the user manager is
  dead (WI#769).** With rootless Podman and a dead `user@<uid>.service`,
  `stop-tenant` keeps the keeper on purpose, prints one line with the recovery
  command, and exits 0 instead of surfacing Podman's bus errors; a teardown
  refuses with the same line rather than reporting a tenant removed while its
  keeper survives.
- **The health pipeline reaches the dead-manager and OpenRC cases (WI#766,
  WI#789).** A dead tenant user manager becomes a `systemd@<tenant>` page
  carrying the recovery command, and an OpenRC component that degrades with
  zero failing services now escalates instead of ending the tick silently.
- **CI storage is bounded (WI#791).** Candidate rootfs tars expire in two
  days, scan evidence in 90, promotion evidence in a year, and
  `ci-config` fails a job that declares artifacts without a bounded
  `expire_in`.

## Sandbox and tunnel networking

- **The relay listener is labeled, and the label is proven per host
  (WI#641).** `ic/src/sandbox/proxy/relay.rs` creates the listener on a
  dedicated thread whose `sockcreate` attribute is set to
  `system_u:system_r:container_t:s0`, then to the full category range; a
  refusal advances to the next candidate, and two refusals end at the
  unlabeled bind. Before a container starts, the daemon connects from a
  socket created under a container-shaped source context and checks `accept`
  plus one byte each way. The attempt is repeated on every sandbox execution,
  so a corrected policy takes effect without a daemon restart; the first
  refusal is a warning and later ones are debug-level. A host without
  `selinuxfs` starts no label thread and binds exactly as before.
- **What the label changes at the SELinux layer.** On the targeted policy,
  any `container_t` process at any level passes the socket check for this
  listener; before the label, no container did on an enforcing host. Other
  processes are kept out by what kept them out before: the private `0700`
  parent directory and the fact that only this run mounts `channel`. The
  relay's `:ro,z` mount and the daemon's refusal to relabel the channel are
  unchanged.
- **The tunnel suite is runtime-neutral (WI#782).** Image presence is read
  with `image inspect`, generated Containerfiles are named `Dockerfile`, and
  the joined-run host-escape probe resolves both `host.containers.internal`
  and `host.docker.internal`. `LUNARWING_E2E_DOCKER_TESTS=1` opens the Docker
  lane, `LUNARWING_E2E_TUNNEL_RUNTIME=podman|docker` may pin the runtime but
  must agree with the open gate, and `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1`
  rejects a host that cannot provide the detached-unit checks before any
  fixture starts.
- **What was run for WI#782.** On 2026-11-20, at the merged head, the gated
  suite reported `14 passed; 0 failed` in 624.16 s on rootless Podman 6.1.4
  (crun 1.28, kernel 6.18.54-gentoo, OpenRC host): the seven live WireGuard
  lanes and the suite's six runtime-free tests, with the NetBird lane skipped
  on its closed gate. The same shape passed on a rootless Docker 29.10.0
  engine started in the same user account, in 564.91 s, with the detached-unit
  checks printing their Docker skip. No systemd service-mode run, no
  tenant-unit tunnel run, no public-cloud run and no CI tunnel job is claimed.
  The record is [`docs/ops/WI782_TUNNEL_LIVE_COVERAGE.md`](docs/ops/WI782_TUNNEL_LIVE_COVERAGE.md).
- **OpenRC's service `PATH` is now measured on Gentoo (WI#770).** OpenRC
  supplies its own list, not the caller's: `openrc-run` drops the caller's
  `PATH` unless `rc_env_allow` names it, prepends its fixed list, and adds
  `/etc/profile.env`. Measured on OpenRC 0.62.6 (Alpine 3.22) and OpenRC
  0.63.3 (Gentoo 2.18), that list already carried `/usr/local/sbin`,
  `/usr/sbin` and `/sbin`, so the rendered init script's append loop added
  nothing. The loop stays as a safeguard for a build that leaves a directory
  out. The three texts that said OpenRC inherits the host `PATH` were
  corrected (`ic/scripts/mt-admin-lib/08-units-openrc.sh`,
  `ic/src/tools/README.md`, `ic/scripts/tests/test-workload-isolation-rendering.sh`).
- **NetBird still refuses `SANDBOX_TUNNEL_DNS` (WI#603).** The refusal is in
  configuration validation, before any container-runtime call
  (`ic/src/sandbox/tunnel_sidecar.rs`), and the reason is unchanged: NetBird
  uses its managed resolver and nameserver groups.

## Agent and LLM runtime

- **A terminal failure names the limits that actually govern the thread
  (WI#783).** `ic/crates/lunarwing_engine/orchestrator/default.py` appends
  bounded remediation lines to the terminal error: the failing action names,
  the no-progress async timeout when that is the class, and the thread's real
  `max_iterations` and `max_consecutive_errors` values. Only a foreground
  thread's cap is labelled `AGENT_MAX_TOOL_ITERATIONS`; mission and research
  threads say "engine default; no environment setting", and the consecutive
  error counter is described as a fact with no setting rather than as
  something to raise.
- **Action names in that text are bounded and sanitized (WI#783).** A model-
  chosen name is cut to 48 ASCII identifier characters, at most four names
  are listed, and role-token openers are broken so a name cannot smuggle
  prompt structure into operator text or a System message.
- **Both failure counters let the promised recovery turn run (WI#783).** A
  code step feeds the no-progress counter and the consecutive-error counter;
  when both trip on the same step, the warning that was already scheduled is
  delivered before the turn ends, so the text's "after a final recovery
  attempt" is true for both terminal classes.
- **Startup and version strings follow the crate version.** The daemon logs
  `Starting LunarWing v2.2.1` from `CARGO_PKG_VERSION`, and the gateway's
  `server_version` is `2.2.1` (the Wingtip wire tests carry the same value).

## Security and supply chain

- **Wasmtime acceptances removed after the records changed (WI#787).**
  Advisory-db `074ea9a7` marked the records `unaffected` below `39.0.0` and
  `47.0.0`; the locked 36.0.x release lies below both, so the entries showed as
  `advisory-not-detected`. They are gone from `ic/deny.toml`, and the comment
  says that a re-broken record is a new maintainer decision, not a copy-paste.
- **Wasmtime moved to `36.0.23` for RUSTSEC-2026-0587.** The advisory,
  dated 2026-11-19 and in the RustSec database since 2026-11-20, is a WASI
  filesystem sandbox escape on Windows through a drive-prefixed path
  component in `wasmtime-wasi`, fixed in `36.0.22`. Both
  lockfiles that carry Wasmtime moved from `36.0.21` to `36.0.23`, which is
  `36.0.22` plus a fix to upstream's release workflow. The manifest floors
  moved to `36.0.22`, the `wasmtime-internal-cache` patch was rebased onto
  `36.0.23`, and `[bans]` now denies `wasmtime-wasi` below `36.0.22` within
  the 36.x line. No advisory was accepted for it.
- **The nine `heapless` advisories are accepted for one graph position
  (WI#781, WI#788).** `heapless 0.7.17` reaches the locks only through
  `postcard 1.1.3`'s default `heapless-cas` feature, pulled by `monty 0.0.17`;
  `postcard` uses only `heapless::Vec` and `heapless::String` outside its
  tests, and no bump clears the advisories because `postcard 1.x` cannot take
  `heapless 0.9.4`. `[bans]` now restricts `heapless <0.9.4` to a `postcard`
  parent and restricts `postcard <2.0.0` to its current direct parents, so a
  future consumer of postcard's default features fails closed rather than
  inheriting the nine acceptances.
- **The audit is a point-in-time record, not a standing claim (WI#784).** The
  report is [`docs/ops/v2.2.1.0_CARGO_DENY_AUDIT.md`](docs/ops/v2.2.1.0_CARGO_DENY_AUDIT.md).
  It records 23 acceptances — 4 under WI#367, 8 under WI#418, 2 under WI#749
  and 9 under WI#781 — all re-verified against RustSec advisory-db
  `074ea9a7` (2026-11-19), with no ignore added, removed, widened or
  re-worded by the audit. The only unaccepted raw-lock vulnerability IDs are
  the two `pyo3` records, whose package never resolves into a compiled build
  graph.
- **The secret-history baseline carries the vendor refresh (WI#778).** The
  2.2.1 sweep moved 78 vendor directories; the ten unpinned fingerprints it
  added are recorded in `.gitlab/gitleaks-history-ignore` and
  `.gitlab/gitleaks-history-dispositions.md`. Without that, the blocking
  `secret-history` job would have failed at the release tag, because the scan
  runs only on schedules and protected tags. No durable schedule exists in
  this repository; the schedule is a project setting.
- **mt-admin refuses a flag-shaped value (WI#637).** `need_value` now rejects
  a value that itself looks like a long flag, so `--with-kimi --target main`
  can no longer swallow `--target` and write it into the tenant env. A secret
  flag's refused value is never echoed. `ic/scripts/import-tenant.sh` carries
  the same function, and a test compares the two bodies.

## Operations and tenant lifecycle

- **A dead user manager no longer produces raw Podman noise on stop
  (WI#769).** `worker_egress_stop_keeper` asks the user-manager state first
  when the runtime is rootless Podman on systemd. With the manager down,
  `stop-tenant` passes `keep`: the keeper pod is retained on purpose, one line
  names `user@<uid>.service` and the `systemctl start` that recovers it, and
  the command exits 0. `remove-tenant` passes no mode and gets a refusal that
  names the same unit, with keeper, network and rendered state left intact for
  retry. Podman is never invoked in that state, so its bus errors cannot leak.
  An `unknown` manager state still falls through to the real call.
- **The health pipeline pages for a dead tenant manager (WI#766).** A
  `systemd@<tenant>` target carries the recovery command into the escalation
  page, and the tenant sweep stays inside its deadline: linger is read from
  logind's marker file rather than by asking logind, and the budget is
  re-checked between the manager probe and the daemon probe. Measured in a
  container with the review's two probes: five healthy lingering tenants
  reported healthy in 1 s where the earlier code took 21 s and degraded; a
  hanging tenant produced a partial report at 23 s instead of being killed by
  the outer `timeout 30` with no JSON.
- **OpenRC self-heal reaches component-level degradation (WI#789).** `openrc`
  is a diagnostic component: zero-service and component-wide degrades now
  reach a monitor-only target, the cron blind spot escalates as
  `openrc@scheduler` with a concrete `rc-service <sched> start` recovery
  command, and a real failing service keeps its single restart path without a
  duplicate page. The report carries `.installed` so the escalation names the
  installed scheduler. New matrix cases B27–B31 pin the shapes. The
  cron-driven pipeline cannot page for its own scheduler failing; the manual
  escalation path and the two-observation grace period are documented.
- **The setup wizard states a refused service instead of hiding it (WI#645).**
  `setup_service_offer` now returns `Offer`, `Refused { reason }` or
  `NotApplicable`. A refused install — including the root-service policy
  refusal and an unsupported `LUNARWING_SERVICE_MANAGER` — is printed with its
  reason and the way out; a host with no supported manager stays quiet in the
  wizard while `lunarwing service` and `lunarwing doctor` keep reporting it.
  An identity-lookup failure surfaces as an error instead of being mistaken
  for a policy refusal.
- **PostgreSQL 16.16 rollout is recorded per host (WI#780).** The inventory
  now separates the 2026-11-17 snapshot from the re-verifications. On
  `ajhermes-z`, `hermes` was 16.14 / `160014` on the floating `pg16` tag and
  is 16.16 / `160016` on the reviewed digest after `refresh-pg-image`; its
  named volume survived and the daemon `/health` returned `200`. `antelope`
  and `barracuda` were already at the pin. `dash` on `colgatev` was refreshed
  from 16.15 to 16.16. A read-only re-check on 2026-11-20 found all three
  `ajhermes-z` Quadlets naming the reviewed digest and no floating `pg16`
  reference. `hermes` keeps a `vector 0.8.6` catalog with `0.8.7` available,
  because the SQL update stays operator-run. The record is
  [`docs/ops/POSTGRESQL-VERSION-INVENTORY.md`](docs/ops/POSTGRESQL-VERSION-INVENTORY.md).
- **The v2.2.0.0 notes gained their security foundation (WI#777).** The
  PostgreSQL 16.16 section and its per-tenant upgrade step were added to the
  previous release's notes after its tag, and the inventory and audit pages
  were extended with the extension-rebuild evidence. Since that text ships on
  this branch, it is the version of the previous notes an operator reading
  Shinobi will see.

## Documentation

- **Architecture and guide refresh (WI#774).** Two passes revalidated every
  document under `docs/architecture/` and `docs/guides/` against the source.
  The corrected texts include the OpenRC `PATH` statements (WI#770), the relay
  SELinux behavior (WI#641), the tunnel guide's runtime matrix (WI#782), the
  worker protocol vocabulary and bump checklist (WI#785), and the multi-tenant
  and self-heal pages (WI#766, WI#789).
- **Worker containers (WI#785).** Kimi Code is pinned at `2.6.3`, OpenCode 2 at
  `v2.3.2` and OpenCode v1 at `v1.19.6`. The OpenCode 2 wrapper now forces
  `OPENCODE_FLAGS=-console_watches,-mcp_events,-artifacts` on every start —
  the last two added here — because upstream resolves that variable before
  every config document, a workspace's own `opencode.json` included. The
  managed `policies`-only document, the compile-out guard and the exposure
  wrapper are unchanged in shape and re-verified at the new pins; the console
  policy evaluator works at `v2.3.2`, and the Kimi `2.6.3` end-to-end guard
  denial is recorded.
- **New operator material.** `docs/ops/WI782_TUNNEL_LIVE_COVERAGE.md`,
  `docs/ops/CI_ARTIFACT_RETENTION.md`, the edge-timeout sections of
  `docs/ops/REGISTRY_PUSH_499.md`, and the re-verification sections of
  `docs/ops/POSTGRESQL-VERSION-INVENTORY.md`.
- **Small docs.** `docs/plans/REORG.md` and `docs/associated_repos/repo_list.md`
  record the repository-housekeeping changes (WI#735, WI#689).

## Compatibility

| Area | v2.2.1.0 |
|---|---|
| Owned packages / core Rust | Eleven packages at `2.2.1`; Rust 2024, MSRV `1.96.1` |
| WASM runtime | Wasmtime/WASI `36.0.23` in `ic/Cargo.lock` (manifest floor `36.0.22`); the `wasmtime-internal-cache` patch stays on that line |
| Channel registry | darkirc `0.1.17`, multica `0.1.21`, weechat `0.2.18`, xmpp `0.1.23` |
| Tool registry | cavepony-tool `0.4.12`, gitlab `0.1.16`, gotify `0.1.8`, llm-context `0.1.10`, multica-bridge `0.1.11`, ssh `0.1.9`, vision-analyze `0.2.3`, voice-transcribe `0.1.9`, web-search `0.2.11` |
| External workers | Kimi Code `2.6.3`, OpenCode `v1.19.6`, OpenCode 2 `v2.3.2`, Pebble `2d0f3786…`, LunarDepth `0472d115…` |
| Worker toolchain | Debian snapshot `20261105T082236Z`; Node `22.23.4` (Kimi, LunarDepth); pnpm `11.7.0` (LunarDepth); Bun `1.4.4` (OpenCode lanes) |
| Sandbox | Linux default `proxy-enforced`; `tunnel` opt-in; relay listener labeled on SELinux hosts with a per-host probe and unlabeled fallback; runtime binaries resolved through an sbin-inclusive `PATH` on systemd and OpenRC tenants |
| Terminal client / local REPL | Wingtip `0.1.0` over HTTP/SSE; daemon Unix socket supports protocol 1 and legacy compatibility responses |
| Advisory baseline | 23 accepted IDs (4 WI#367, 8 WI#418, 2 WI#749, 9 WI#781); RUSTSEC-2023-0071 cleared; the two Wasmtime acceptances removed (WI#787) |

Registry bumps require rebuilt WASM artifacts. Rebuild every enabled worker
image through the protected candidate, scan and promotion path; local builds
do not replace those gates.

## Upgrade Notes

1. **Reinstall the extensions the registries moved.** Rebuild the daemon and
   the XMPP bridge, reinstall the four channel and nine tool extensions, and
   rebuild enabled worker images. `./ic/scripts/build-wasm-extensions.sh`
   builds the WASM channels and tools from this tree; the registry entries
   expect the versions in the table above.
2. **Rebuild worker images at the new pins.** Kimi Code `2.6.3`, OpenCode 2
   `v2.3.2` and OpenCode v1 `v1.19.6` are the pins in this tree. Build them
   through the protected candidate, scan and promotion jobs; the candidate
   artifacts now expire after two days, so play a manual `worker-*-promote`
   job from the pipeline that produced its candidate. If that pipeline's
   artifacts are gone, re-run the candidate and then the scan.
3. **Re-check `SANDBOX_NETWORK` on an SELinux host.** Nothing changes for a
   host with SELinux disabled or permissive: the relay binds as before. On an
   enforcing host the relay now tries a labeled listener and falls back to the
   unlabeled bind if the policy refuses every candidate. If sandbox runs still
   fail at relay startup, `sudo ic/scripts/lunarwing-sandbox-proxy-diag.sh
   <tenant> --watch` shows the candidate denials; the daemon warns once per
   process with the kernel's error for each refused candidate.
4. **Refresh a tenant's PostgreSQL image if you have not already.** A daemon
   upgrade still does not move an existing tenant's database image. Per
   tenant, sequentially:
   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh pg-image-status <tenant>
   sudo ic/scripts/lunarwing-mt-admin.sh refresh-pg-image <tenant>
   sudo ic/scripts/lunarwing-mt-admin.sh pg-image-status <tenant>
   ```
   The refresh fails closed before stopping services on a failed pull, on a
   non-16 image, and on `server_version_num` below `160016`. It does not run
   `ALTER EXTENSION vector UPDATE`; a `0.8.6` catalog stays `0.8.6` while the
   replacement image's `0.8.7` library serves queries, and
   `psql <tenant> -v ON_ERROR_STOP=1 -c 'ALTER EXTENSION vector UPDATE'` is
   the operator-run step if you want the catalog comment change.
5. **Leave the advisory acceptances as they are.** `ic/deny.toml` carries 23
   IDs. The two Wasmtime entries are gone because the records changed; if a
   gate reports one of them as `advisory-not-detected`, do not re-add it.
   `cargo-deny` on this tree should pass with no warnings.
6. **Create or keep the secret-history schedule.** The blocking scan runs on
   schedules and protected tags only, and the schedule is a project setting,
   not committed YAML. If the project has none, create one under **CI/CD >
   Schedules** so an unpinned fingerprint is caught before the next tag. The
   ten fingerprints the 2.2.1 vendor refresh added are already pinned.
7. **Do not re-add the `container-build` tag to shared runner 17 yet.** It was
   removed on 2026-11-19 because the GitLab edge cuts an upload at 60 seconds;
   re-add it only after a rate-limited upload through the affected route and
   entry point succeeds. The edge-side change is a host-administrator action.
8. **Run old mt-admin commands to completion before upgrading.** A host
   upgraded from an older mt-admin keeps its stale `/run/lock/...` lock
   directories; the locks are `flock`-only, so nothing migrates, but a command
   still running from the old version holds the old path until it exits. On
   systemd, run `sudo ic/scripts/lunarwing-mt-admin.sh init-lock-tree` once so
   the boot-time tmpfiles drop-in exists.
9. **Re-check `LUNARWING_SERVICE_MANAGER` before the next tenant command.** A
   value that contradicts the host's PID-1 markers is refused before any
   command writes. `LUNARWING_SERVICE_MANAGER_FORCE=1` is only for an image
   build or chroot, and it is refused while the ports registry holds a tenant.

## Known Limitations

- **NetBird custom DNS is unsupported (WI#603).** Setting `SANDBOX_TUNNEL_DNS`
  with the NetBird provider is refused during configuration validation, before
  any container-runtime call. Use NetBird nameserver groups reachable through
  the overlay; an upstream outside the overlay fails closed because its
  resolver sockets are unmarked
- **Harness redaction remains bounded and best-effort.** Offset or grouped
  dumps, other encodings, arbitrary fragments and unmarked truncation are not
  comprehensively covered. The external-worker isolation boundary remains
  per-task-container authority; see
  `docs/architecture/HARNESS_WORKER_CAPABILITIES.md`.
- **Wingtip socket work is still M4.** The daemon Unix socket supports
  protocol 1, but Wingtip's own socket transport and reconnect suite remain
  future work. A missing or replaced daemon socket is logged without
  automatic rebind.
- **RSA policy stops at the shared client.** Worker-mode OpenSSH and
  `ssh_git`'s post-pin OpenSSH transport retain their own host-key algorithm
  policies. RSA private keys are rejected on every LunarWing SSH path; RSA
  host keys are unsupported only on the shared `russh` paths.

忍び
