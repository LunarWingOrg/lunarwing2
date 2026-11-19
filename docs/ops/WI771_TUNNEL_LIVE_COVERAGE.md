# WI#771 — Sandbox tunnel live coverage: Docker fallback and systemd service mode

**Date:** 2026-11-19 (SweetieBot, branch `sweetiebot/kimi-wi771-20261119-0505`).

**Work item:** `lunarwing/lunarwing2#771`, carrying the v2.2.0.0 Known
Limitation that the gated tunnel suite was only ever exercised on rootless
Podman, with Docker fallback harness-covered and no systemd-service-mode run.

## What changed in the tree

- `ic/tests/podman_sandbox_tunnel_integration.rs` is no longer Podman-only.
  The lane is opened by `LUNARWING_E2E_TUNNEL_TESTS=1` plus exactly one runtime
  gate: `LUNARWING_E2E_PODMAN_TESTS=1` (existing) or
  `LUNARWING_E2E_DOCKER_TESTS=1` (new, the same flag
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
- Detached-helper checks are tied to real systemd service mode via
  `WorkloadSpawner::uses_systemd_services()` instead of probing for a
  `systemctl` binary. `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` turns the skip
  into a failure. The lane now also asserts the transient unit
  `lunarwing-tunnel-<instance>` **exists** while the sidecar runs, so the
  "gone afterwards" check cannot pass vacuously; a service-mode host whose user
  manager does not answer fails rather than skips. The `pasta`-in-unit scan is
  Podman-only (Docker has no pasta helper).
- "Nothing left behind" is now asserted, not assumed: the lifecycle and
  concurrent-start lanes require that the runtime reports no container carrying
  `lunarwing.sandbox-tunnel` after teardown.

## What was run here, and what was not

This development host is an **OpenRC** machine with **rootless Podman 6.1.4
(crun)** and **no Docker binary** and **no `systemctl`**. That is the same class
of host the v2.2.0.0 limitation was measured on. Consequently:

- **Podman regression run (passed).**
  `LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_PODMAN_TESTS=1` with
  `tunnel_sidecar_wireguard_lifecycle_and_join`: `1 passed; 0 failed …
  finished in 92.61s`. Afterwards `podman ps -a --filter
  label=lunarwing.sandbox-tunnel` reported no containers and no `lunarwing`
  volumes remained. Purpose: show the refactor is neutral on the runtime the
  previous release was validated on.
- **Service-mode guard (verified).** Re-running the same lane with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` fails with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1 but the workload spawner is not in
  systemd service mode`. The flag is therefore not a no-op: a host without
  service mode cannot record a pass by accident.
- **Docker fallback: not run live here.** The Docker lane exists and is gated,
  but no host with the Docker CLI was available. The matching acceptance box
  stays open for the maintainer; the run is
  `LUNARWING_E2E_TUNNEL_TESTS=1 LUNARWING_E2E_DOCKER_TESTS=1` against
  `--test podman_sandbox_tunnel_integration`.
- **systemd service mode: not run live here.** This host has no user manager,
  so the detached-unit checks report their skip; with
  `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` they fail by design. A service-mode
  host must run with that flag set to record a pass.
- **Tenant-unit run with `SANDBOX_NETWORK=tunnel`: not possible here.** It
  needs the same systemd user-manager host.

## Replacement paragraph for the v2.2.1.0 release notes

> **Tunnel live coverage.** The gated tunnel suite runs on rootless Podman
> (WI#592/WI#600 validation: Podman 5.8.2 with NetBird 0.82.0 and a real
> WireGuard peer) and, since WI#771, on the Docker CLI fallback through a
> separate `LUNARWING_E2E_DOCKER_TESTS` lane; both lanes are off by default.
> Docker live results are recorded on WI#771 when a Docker host runs the lane.
> The detached systemd helper-unit checks are now bound to real service mode and
> can no longer pass vacuously: they fail under
> `LUNARWING_E2E_TUNNEL_REQUIRE_SYSTEMD=1` when service mode is absent, and they
> first prove the transient unit exists before requiring its removal. A tenant
> unit in service mode with `SANDBOX_NETWORK=tunnel` is a deployment check, not
> a claimed CI result: no public-cloud run, no CI tunnel job and no
> service-mode live run is claimed by this release.
