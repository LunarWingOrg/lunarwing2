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
- **The detached-unit checks apply only where the runtime keeps the helper in
  the client's unit (AGJ-1, AGJ-3).** `detached_unit_check_applies` returns true
  for rootless Podman and false for Docker. Rootless Podman leaves `conmon` and
  `pasta` in the detached `run` client's transient unit
  `lunarwing-tunnel-<instance>`, so the unit stays loaded and is observable. A
  `docker run -d` client leaves nothing in its unit, so the unit is collected
  as soon as the client exits; on Docker the checks print a skip instead of
  failing. `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` on Docker now fails with
  that reason, because the requirement cannot be met there.
- **Every test that requires the unit gone first requires it present.** The
  lifecycle test asserts both sidecars' units exist while they run, the
  concurrent-start test asserts its sidecar's unit exists, and the NetBird test
  asserts the primary's unit exists. All three then check removal. So "gone
  afterwards" cannot pass vacuously in any of them, not just in one (AGJ-3).
- **"Nothing left behind" covers sidecars and joined runs, scoped to the lane
  (AGJ-4).** `tunnel_and_joined_containers` queries `lunarwing.sandbox-tunnel`
  (sidecars) and `lunarwing.sandbox` (joined runs) separately and unions the
  results. Each lane takes a `leftover_baseline` before it creates anything and
  fails only on containers that were not in that baseline, so a long-lived
  sidecar of another daemon, or a second test running in parallel, cannot make
  the check fail.
- **A failed command is reported before the cleanup checks (AGJ-6).** The
  concurrent-start test reads and asserts both command results *before* the
  helper-unit and leftover checks, and the lifecycle test reads its `result`
  before them, so a real lifecycle error is no longer reported as a missing
  unit or a leftover container.

## What was run here, and what was not

This development host is an **OpenRC** machine (PID 1 is `init`, no
`systemctl`) with **rootless Podman 6.1.4 (crun)** and **no Docker binary**.
That is the same class of host the v2.2.0.0 limitation was measured on.

- **Podman suite run at this head: passed, 10 of 10.** On this host,
  `LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_PODMAN_TESTS=1
  LUNARWING_E2E_TUNNEL_BASE_IMAGE=quay.io/libpod/alpine:latest` with
  `--test podman_sandbox_tunnel_integration -- --nocapture --test-threads=1`
  reported `test result: ok. 10 passed; 0 failed; 0 ignored` in 625.16 s. The
  NetBird test skipped because its gate was closed. Afterwards
  `podman ps -a` reported no containers and `podman volume ls` no `lunarwing`
  volume, so the scoped leftover check ran and passed rather than being
  vacuous. The detached-unit checks printed their service-mode skip, as
  designed on this host.
- **Service-mode requirement verified, not just wired.** Re-running
  `tunnel_concurrent_first_commands_keep_firewall_stable` on this host with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` fails at
  `tests/podman_sandbox_tunnel_integration.rs:418` with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1 but the workload spawner is not in
  systemd service mode, so the detached-unit-in-use check cannot run`
  (`test result: FAILED. 0 passed; 1 failed; … 9 filtered out`), so a host
  without service mode cannot record a pass by accident. Without the flag the
  same lanes print the skip and pass.
- **Docker fallback: not run live here.** No host with the Docker CLI was
  reachable to this agent. The Docker lane exists and is gated; the matching
  acceptance box stays open for the maintainer. The run is
  `LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_DOCKER_TESTS=1` against
  `--test podman_sandbox_tunnel_integration`. On Docker the detached-unit
  checks print a skip by design (AGJ-1), and
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` fails there with that reason.
- **systemd service mode: not run live here.** This host has no user manager,
  so the detached-unit checks report their skip; with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` they fail by design. A service-mode
  host must run with that flag set to record a pass. The `ajhermes-z` host
  named in the work item is visible on the NetBird overlay but refused this
  agent's SSH keys, so it could not be used.
- **Tenant-unit run with `SANDBOX_NETWORK=tunnel`: not possible here.** It
  needs the same systemd user-manager host.

## Replacement paragraph for the v2.2.1.0 release notes

> **Tunnel live coverage.** The gated tunnel suite has been run on rootless
> Podman only: 9 of 9 tests on Podman 5.8.2 (2026-11-11), including NetBird
> 0.82.0 and a real WireGuard peer, and **10 of 10 at the WI#782 head on Podman
> 6.1.4 (2026-11-19)** with the NetBird gate closed. A Docker CLI lane
> (`LUNARWING_E2E_DOCKER_TESTS=1`) exists and is off by default, but **no Docker
> run is claimed**. The detached systemd helper-unit checks are bound to
> rootless Podman in systemd service mode: they first prove the transient unit
> `lunarwing-tunnel-<instance>` exists before requiring its removal, so they
> cannot pass vacuously there. On Docker no unit outlives the `docker run -d`
> client, so those checks print a skip and nothing about the unit is proven.
> **No service-mode run, no tenant-unit tunnel run, no public-cloud run and no
> CI tunnel job is claimed by this release.**
