# WI#782 — Sandbox tunnel live coverage: Docker fallback and systemd service mode

**Date:** 2026-11-19 (SweetieBot, branch `sweetiebot/kimi-wi782-20261119-1905`).

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
`systemctl`) with **rootless Podman 6.1.4 (crun)** and **no Docker binary**.
That is the same class of host the v2.2.0.0 limitation was measured on.

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
  rejected before the runtime was called. This is not new live coverage.
- **Docker fallback: not run live here.** No approved Docker test host was
  available. The Docker lane exists and is gated. The run is
  `LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_DOCKER_TESTS=1` against
  `--test podman_sandbox_tunnel_integration`. On Docker the detached-unit
  checks print a skip by design (AGJ-1), and
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` fails there with that reason.
- **systemd service mode and tenant-unit run: not run live here.** No approved
  test host with a systemd user manager was available. Acceptance boxes 1–3
  stay open for the maintainer.

## Replacement paragraph for the v2.2.1.0 release notes

> **Tunnel live coverage.** The gated tunnel suite has been run on rootless
> Podman only: 9 of 9 tests on Podman 5.8.2 (2026-11-11), including NetBird
> 0.82.0 and a real WireGuard peer, and **10 reported passes on Podman 6.1.4
> (2026-11-19, before the WI#782 review follow-up)** with the NetBird gate
> closed. A Docker CLI lane (`LUNARWING_E2E_DOCKER_TESTS=1`) exists and is off by
> default, but **no Docker run is claimed**. The detached systemd helper-unit
> checks run for Podman in systemd service mode: they first prove the transient unit
> `lunarwing-tunnel-<instance>` exists before requiring its removal, so they
> cannot pass vacuously there. On Docker no unit outlives the `docker run -d`
> client, so those checks print a skip and nothing about the unit is proven.
> **No service-mode run, no tenant-unit tunnel run, no public-cloud run and no
> CI tunnel job is claimed by this release.**
