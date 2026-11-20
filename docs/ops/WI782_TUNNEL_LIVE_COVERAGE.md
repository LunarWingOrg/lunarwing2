# WI#782 — Sandbox tunnel live coverage: Docker fallback and systemd service mode

**Date:** 2026-11-19, with the merged-head live run, the AHN-1 wording
correction, and the Docker live run added 2026-11-20
(SweetieBot; the tree landed from branch `sweetiebot/kimi-wi782-20261119-1905`
as squash `afa2998a9`, and the run was recorded from branch
`sweetiebot/kimi-wi782-20261120-0005`).

**Work item:** `lunarwing/lunarwing2#782`, which continues `#771` and carries the
v2.2.0.0 Known Limitation that the gated tunnel suite was only ever exercised
on rootless Podman, with Docker fallback harness-covered and no
systemd-service-mode run. It also carries the review findings AGJ-1 to AGJ-5 of
merge request !2568, the first attempt, which was closed unmerged.

## What changed in the tree

- `ic/tests/podman_sandbox_tunnel_integration.rs` is no longer Podman-only.
  The lane is opened by `LUNARWING_E2E_TUNNEL_TESTS=1` plus exactly one runtime
  gate: `LUNARWING_E2E_PODMAN_TESTS=1` (existing) or
  `LUNARWING_E2E_DOCKER_TESTS=1` (new for this suite, the same flag
  `generic_sandbox_acceptance.rs` already uses). Opening both gates is refused
  instead of guessed. `LUNARWING_E2E_TUNNEL_RUNTIME=podman|docker` may pin the
  runtime but must agree with the open gate. Every test function lost its
  runtime-specific name (`podman_tunnel_*` → `tunnel_*`), and the joined run
  asserts that `CliRunner` resolved to the lane's runtime rather than falling
  back to the other CLI.
- Runtime-neutral primitives: image presence is checked with
  `image inspect` (both CLIs accept it) instead of Podman's `image exists`, and
  generated Containerfiles are written as `Dockerfile`, which both builders
  read. The joined-run host-escape probe now resolves
  `host.containers.internal` **and** `host.docker.internal`, then falls back to
  the default gateway, so the assertion is not vacuous on Docker.
- **The detached-unit checks run for Podman in systemd service mode
  (AGJ-1, AGJ-3, AHB-4).** The preflight checks the selected runtime and
  spawner mode; it does not detect whether Podman is rootless. Rootless Podman
  leaves `conmon` and `pasta` in the detached `run` client's transient unit
  `lunarwing-tunnel-<instance>`, so the unit stays loaded and is observable. A
  `docker run -d` client leaves nothing in its unit, so the unit is collected
  as soon as the client exits; on Docker the checks print a skip instead of
  failing. `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` rejects Docker or a non-service
  spawner before starting any test fixtures.
- **A passing service-mode lane proves presence and removal (AGJ-3).** The
  lifecycle test checks both sidecars' units while they run, the concurrent-start
  test checks its sidecar's unit, and the NetBird test checks its primary's unit.
  All three check removal during teardown, even if an earlier check fails.
- **The leftover check covers new sidecar and sandbox names (AGJ-4, AHB-3).**
  `tunnel_and_joined_containers` queries `lunarwing.sandbox-tunnel`
  (sidecars) and `lunarwing.sandbox` (joined runs) separately and unions the
  results. Each lane takes a `leftover_baseline` before it creates anything and
  fails on containers that were not in that baseline. The difference does not
  identify which process created them. Use `--test-threads=1` and a dedicated
  account and container store with no live LunarWing daemon, other sandbox runs,
  or parallel suite invocation. Run the tenant-unit check in a dedicated
  tenant, not alongside the suite.
- **Fixture teardown runs before the lane reports failure (AGJ-6, AHB-2).**
  An in-use unit check returns an error rather than panicking. The lane awaits
  manager shutdown or sidecar stop (and NetBird peer and volume removal), then
  verifies teardown without replacing the original error. Nested test assertion
  panics also run through teardown before being propagated.

## What was run here, and what was not

This development host is an **OpenRC** machine (PID 1 is `init`, no
`systemctl`) with **rootless Podman 6.1.4 (crun 1.28)**, kernel
6.18.54-gentoo, and **no Docker binary in the base image**. That is the same
class of host the v2.2.0.0 limitation was measured on. For the Docker lane a
**rootless Docker 29.10.0** engine was started in the user account itself
(overlayfs storage driver, `pasta` network driver, no cgroups because systemd
is absent); it is a genuine `dockerd`, not a Docker-compatible shim over
Podman. The merged-head Podman run below was taken at `0b411a697`; the Docker
run was taken at `4192ee72b`, where the test file is unchanged.

- **Merged-head Podman suite run: 14 passes (AHI-1, 2026-11-20).** The suite
  was run live at the head the branch carries, after the teardown rewrite that
  the earlier 10-pass run predates. On the same host, with
  `LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_PODMAN_TESTS=1
  LUNARWING_E2E_TUNNEL_BASE_IMAGE=quay.io/libpod/alpine:latest` and
  `--test podman_sandbox_tunnel_integration -- --nocapture --test-threads=1`,
  the run reported `test result: ok. 14 passed; 0 failed; 0 ignored` in
  624.16 s. Every live WireGuard lane ran — lifecycle and join, custom DNS,
  hostname endpoint, dead peer, healthy peer death, cold-start budget,
  concurrent first commands — and the teardown wrapper's two regression tests
  ran with them. The NetBird test skipped because its gate was closed. The
  detached-unit checks printed their service-mode skip, as designed on this
  host. Afterwards `podman ps -a` reported no container carrying
  `lunarwing.sandbox-tunnel` or `lunarwing.sandbox` and `podman volume ls` no
  `lunarwing` volume, so the scoped leftover assertions ran and passed rather
  than being vacuous. This is the run AHI-1 asked for: a failure in the
  maintainer's Docker or service-mode runs can no longer be mistaken for a
  fault of the rewrite.
- **Gate-closed regression at the same head.** The same target with no gates
  opened reported `14 passed; … finished in 0.00s`, so no lane runs by default
  and CI's shape stays green.
- **Service-mode requirement at the same head.** Re-running
  `tunnel_concurrent_first_commands_keep_firewall_stable` with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` fails before any fixture starts, at
  `tests/podman_sandbox_tunnel_integration.rs:922`, with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1 but the detached-unit checks cannot
  run: the workload spawner is not in systemd service mode`
  (`test result: FAILED. 0 passed; 1 failed; … 13 filtered out`, 0.00 s). A host
  without service mode therefore cannot record a pass by accident, and the
  rejection costs no container start.
- **Original Podman suite run, before the review follow-up: 10 reported passes.**
  On this host,
  `LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_PODMAN_TESTS=1
  LUNARWING_E2E_TUNNEL_BASE_IMAGE=quay.io/libpod/alpine:latest` with
  `--test podman_sandbox_tunnel_integration -- --nocapture --test-threads=1`
  reported `test result: ok. 10 passed; 0 failed; 0 ignored` in 625.16 s. The
  NetBird test skipped because its gate was closed. Afterwards
  `podman ps -a` reported no containers and `podman volume ls` no `lunarwing`
  volume, so the scoped leftover check ran and passed rather than being
  vacuous. The detached-unit checks printed their service-mode skip, as
  designed on this host.
- **Original service-mode requirement check, before the review follow-up.** Re-running
  `tunnel_concurrent_first_commands_keep_firewall_stable` on this host with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` fails at
  `tests/podman_sandbox_tunnel_integration.rs:418` with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1 but the workload spawner is not in
  systemd service mode, so the detached-unit-in-use check cannot run`
  (`test result: FAILED. 0 passed; 1 failed; … 9 filtered out`), so a host
  without service mode cannot record a pass by accident. Without the flag the
  same lanes print the skip and pass. The follow-up moves this requirement
  check ahead of fixture startup.
- **Review follow-up validation:** The test target and Clippy passed with
  live gates closed, including regressions for teardown after an error and a
  panic. With stub container CLIs, both unsupported service-mode cases were
  rejected before the runtime was called. The live coverage that answers AHI-1
  at the merged head is the 14-pass run above, not this gate-closed check.
- **Docker fallback, live (2026-11-20).** The full gated suite was run against
  rootless Docker 29.10.0 on this host:

  ```
  # PATH and DOCKER_HOST point at the rootless engine's CLI and socket.
  LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_DOCKER_TESTS=1 \
  LUNARWING_E2E_TUNNEL_RUNTIME=docker \
  LUNARWING_E2E_TUNNEL_BASE_IMAGE=quay.io/libpod/alpine:latest \
    taskset -c 0-17 cargo test -j18 --test podman_sandbox_tunnel_integration \
    -- --nocapture --test-threads=1
  ```

  `test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
  finished in 564.91s`. All seven live WireGuard lanes passed on the Docker
  CLI: lifecycle and join, custom DNS closes uplink, hostname endpoint, dead
  peer, healthy peer death, cold-start budget, and concurrent first commands,
  together with the teardown wrapper's six runtime-free tests. The NetBird lane
  skipped on its closed gate. The lifecycle lane therefore ran its real
  Docker assertions: the joined run uses the verified container ID, the
  sidecar's input/output firewall is listed and checked rule by rule, tampered
  chains and a dead peer fail closed, image-ID rotation recreates the sidecar,
  and the post-teardown check finds no leftover container. The suite built
  `localhost/lw-tunnel-e2e:latest` from `quay.io/libpod/alpine:latest` with the
  Docker builder, and its rotation case rebuilt the tag, so both Docker build
  paths ran.
- **Detached-unit checks on Docker: skipped, as AGJ-1 designed.** Both lanes
  that reach them printed `skipping detached-unit checks: Docker's detached
  run client leaves no process in its transient unit`. Re-running
  `tunnel_concurrent_first_commands_keep_firewall_stable` with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` failed before any fixture started,
  at `tests/podman_sandbox_tunnel_integration.rs:922`, with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1 but the detached-unit checks cannot
  run: Docker's detached `run` client leaves no process in its transient unit`
  (`test result: FAILED. 0 passed; 1 failed; … 13 filtered out`, 0.00 s). A
  Docker host therefore cannot record a unit pass, and the requirement is not
  a no-op.
- **Docker post-run state.** `docker ps -a` listed no container and
  `docker volume ls` no volume afterwards; the scoped leftover assertions also
  ran and passed inside the lanes. The built images remain in the Docker store
  (`localhost/lw-tunnel-e2e:latest` and the base image) because the lane builds
  them, as on the Podman host.
- **Gate-closed regression at the same build.** Without the gates the same
  target reported `14 passed; … finished in 0.00s`, so no lane runs by default
  and CI's shape is unchanged.
- **systemd service mode and tenant-unit run: not run live here.** No approved
  test host with a systemd user manager was available. Acceptance boxes 2–3
  stay open for the maintainer; box 1's Docker run is recorded above.
- **Review follow-up, AHN-1 (2026-11-20).** The approval of !2600 noted that the
  release paragraph below counted "14 of 14", which is the number of test
  functions in the file, not live coverage: seven live WireGuard lanes, six
  tests that need no runtime, and the NetBird lane, which skips while its gate
  is closed. The paragraph and the guide's matrix row now say what ran — the
  seven live WireGuard lanes and the six runtime-free tests passed at the
  merged head, and NetBird was not run. No test code changed for this wording
  correction, so the merged-head run still applies to the file as it stands.

## Replacement paragraph for the v2.2.1.0 release notes

> **Tunnel live coverage.** The gated tunnel suite has been run on rootless
> Podman and on rootless Docker: 9 of 9 tests on Podman 5.8.2 (2026-11-11),
> including NetBird 0.82.0 and a real WireGuard peer; **10 reported passes on
> Podman 6.1.4 (2026-11-19, before the WI#782 review follow-up)** with the
> NetBird gate closed; **the seven live WireGuard lanes passed at the merged
> head of WI#782 on Podman 6.1.4 (2026-11-20)**, after the teardown rewrite,
> together with the suite's six runtime-free tests, while **NetBird was not
> run** (gate closed) and the detached-unit checks were skipped for want of
> systemd service mode; and **the seven live WireGuard lanes and the six
> runtime-free tests passed on rootless Docker 29.10.0 (2026-11-20)**, where
> NetBird was not run and the detached-unit checks skipped because no unit
> outlives a `docker run -d` client. Both WI#782 runs used
> `--test-threads=1`.
> The detached systemd helper-unit checks run for Podman in systemd service
> mode: they first prove the transient unit
> `lunarwing-tunnel-<instance>` exists before requiring its removal, so they
> cannot pass vacuously there. On Docker no unit outlives the `docker run -d`
> client, so those checks print a skip and nothing about the unit is proven.
> **No service-mode run, no tenant-unit tunnel run, no public-cloud run and no
> CI tunnel job is claimed by this release.**
