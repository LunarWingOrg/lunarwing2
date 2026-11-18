# Release Notes for LunarWing v2.2.0.0

## Codename: Shinkai (深海, しんかい) — The Deep Sea

**Release Date:** Unreleased

> Three releases sank.<br>
> Umioto, Kaitei, then —<br>
> the floor was a door. 深海

*The surface moves. The seabed carries it. Past the seabed, the water keeps going.*

*Shinkai verses: Applejack's ledger, WI#591.*

Umioto listened; Kaitei held. **Shinkai goes past the floor.** This release commissions LunarDepth as a new external worker, gives the Podman sandbox a kernel-enforced default egress tier and an opt-in tunnel tier, and closes the RSA advisory by removing RSA from the SSH path entirely. Around those three, tenant upgrades become image-aware and fail-closed, the sandbox image carries a full Python development suite, secret-history scanning becomes blocking, and the crate, worker and documentation baselines advance together.

**Draft status.** This is the second draft of the v2.2.0.0 notes, built from the 2.2.0.0 goals board and the merged work on the integration head. Since the first draft it adds the runtime `PATH` fix and the mt-admin service-manager guard, records the tunnel guide's expansion and its corrections, the worker protocol vocabulary, and the secret-history acceptance evidence. Before the release tag, re-check every version pin, pipeline citation and known limitation against the exact release tree.

## Highlights

- **LunarDepth is commissioned (WI#316, WI#605, WI#606).** The fifth external-worker lane runs the DeepSeek Harness fork from a pinned source ref (`0472d115e5…`) with Node `22.23.4` and pnpm `11.7.0`. Its bridge and task roles are non-root, its profile manifest is generated from the pinned source and gated on an unchanged hash and schema, and its kernel confinement requires Landlock ABI 5 (Linux 6.10). Dispatch stays daemon → in-container bridge → harness API on task loopback, with no self-exposure and the per-tenant egress allowlist intact.
- **The sandbox default becomes kernel-enforced (WI#537).** Linux and new mt-admin tenant environments default to `proxy-enforced`: `--network none` plus a per-execution Unix relay to the allowlist proxy. There is no direct egress path, no credential injection, and non-public destinations are refused. Other platforms keep `none`, and every operator-set tier survives mt-admin re-render.
- **Tunnel networking is opt-in and egress-only (WI#248, WI#592).** `SANDBOX_NETWORK=tunnel` adds a NetBird or WireGuard sidecar whose nftables policy drops all new inbound flows and permits only provider control traffic, overlay egress and the documented DNS path. Provider health and both firewall chains are verified before every run; a failed probe fails closed.
- **RSA leaves the SSH path (WI#368, WI#616).** The `russh` RSA feature is gone, `rsa`, `pkcs1` and `crypto-primes` are out of the dependency graph and vendor tree, and RUSTSEC-2023-0071 is no longer accepted. The shared client offers only Ed25519 and ECDSA host keys, and an RSA-only server now produces an error that names the RSA host key instead of a misleading signature failure.
- **Tenant upgrades rebuild the sandbox image (WI#596).** `upgrade-tenant` detects a changed Dockerfile or Debian snapshot, builds a candidate before stopping anything, promotes it with the daemon and worker builds, and keeps the old image and its rollback ID when the candidate fails.
- **mt-admin refuses a service-manager override the host does not run (WI#730, WI#757).** An explicit `LUNARWING_SERVICE_MANAGER` that contradicts the host's PID-1 markers is refused before any command's first write, naming what was asked, what the host runs and the marker path. `LUNARWING_SERVICE_MANAGER_FORCE=1` is the image-build and chroot escape hatch, and it is refused once the ports registry holds any tenant — an entry alone counts — with an unreadable registry treated as tenants present. The health pipeline records only the manager the host probe reports, even under force.
- **Runtime discovery sees the sbin directories (WI#663).** `could not find pasta` came from a tenant unit `PATH` that named only bin directories, so a rootless runtime and its helpers installed under sbin were invisible. Runtime selection, the podman version probe, the Docker rootless probe, the runner's spawn sites and the reaper now share one path policy that appends the missing sbin directories without reordering or repeating; the systemd tenant unit lists them after the bin directories and before the tenant-writable cargo directory, and the OpenRC init script appends them to the host `PATH` at start.
- **Secret history is blocking (WI#575, WI#590, WI#599).** The full-history scan is blocking on schedules and protected release tags; merge requests run the scanner's regression harness (`secret-history-regression`), not a scan of the history. The recorded replay scanned 11,524 commits and 2.07 GB with no leaks, and the pending-findings list is empty.
- **Retry budgets bound token storms (WI#578, WI#595).** Billed-empty stream retries now carry a cumulative per-leg input-token budget, defaulting to `100000`, with the first transient attempt exempt. `0` restores the old retry-count-only behaviour.
- **Worker lanes advance (WI#609, WI#665, WI#745).** Kimi Code moves to `2.5.3`, OpenCode 2 to `v2.2.7` with a managed Console deny policy and policy-precedence checks, and OpenCode v1 remains current at `v1.19.5`. All exposure guards and compile-out gates remain in force.
- **The crate and advisory baseline moves together (WI#749).** Eleven owned packages are at `2.2.0`, every channel and tool registry entry is bumped, 25 manifest-bearing lock roots are refreshed, and the advisory baseline now records 14 accepted IDs, including the two informational chrono notices; RUSTSEC-2023-0071 is cleared.

## The LunarDepth worker

LunarDepth joins Kimi, OpenCode, OpenCode 2 and Pebble as an opt-in external-worker lane. It is dispatched through the generic external-worker backend with `create_job`; it is not an extension, MCP server, skill, routine or SSH host.

- **Source and build provenance.** The lane builds from a full 40-hex pinned ref, verifies the checkout against it, resolves both `@lunardepth/dsh-lunarwing-worker-app` and `@lunardepth/dsh-lunarwing-worker` from the deployed closure, and never runs `tsx` at runtime. The profile template comes from the pinned source's `initProfile`; no handwritten profile manifest exists.
- **Confinement.** The image runs `landlock-run` built with musl and enforces bwrap and Landlock inside the task. `danger-full-access`, `dsh-bash-local` and `runnerCommand` are forbidden. The kernel floor is Linux 6.10 / Landlock ABI 5 and is checked rather than assumed.
- **Identity and filesystem.** The task and bridge use distinct non-root identities (`10005` and `10105`), a root-owned launch directory, a regenerated home configuration, and task-private temporary directories outside the harness home. Two hostile restarts do not preserve a planted schema or `.env`.
- **Dispatch and exposure.** The only instruction path is daemon job dispatch → in-container bridge → task-runner Unix socket → the harness API on `127.0.0.1`. The lane opens no inbound path, uses no share service, and runs on the per-tenant egress network.
- **Operations.** mt-admin builds, starts, upgrades and rolls back the lane on systemd and OpenRC, and the ports registry carries its `lunardepth_wss` slot. `worker-smoke`, the listener audit and the live egress audit cover the lane; protected candidate, exposure-verify, clean-rebuild digest, scan and promotion jobs gate its images.
- **Build evidence.** A cached and an uncached build produced identical contents and symlink targets across **42,803 rootfs entries**, with matching ownership and modes. Local Podman still produced different whole-image IDs from its layer-packaging metadata; the protected BuildKit digest comparison is the CI gate.

## Sandbox and tunnel networking

The sandbox now has four explicit tiers, and the platform default is no longer "no network" on Linux.

| `SANDBOX_NETWORK` | Intended use | Boundary |
|---|---|---|
| `none` | no network | kernel-enforced `--network none` |
| `proxy-enforced` (Linux default) | allowlisted public HTTP(S) | network-none plus Unix relay to the validating proxy; zero credential mappings |
| `tunnel` | NetBird or WireGuard overlay egress | nftables in the sidecar netns; new inbound dropped; egress only through the tunnel, its marks and its DNS |
| `proxy` | legacy public HTTP(S) | advisory environment proxy; direct bypass remains possible |

- **Default egress (WI#537).** The default allowlist covers package registries, language documentation, GitHub, and the OpenAI, Anthropic and LunarWing Cloud API hosts. The proxy validates destinations, pins DNS and refuses non-public addresses; allowlisted names cannot reach host loopback, private networks, another tenant or cloud metadata. Destination filtering does not restrict HTTP methods.
- **Legacy `proxy` stays explicit.** It remains the only tier whose credential mappings can inject `LUNARWING_CLOUD_API_KEY` or a provider key into plain-HTTP requests. `proxy-enforced` is built with no mappings, so untrusted sandbox code cannot trigger daemon credential injection.
- **Sidecar lifecycle (WI#592, WI#602, WI#604, WI#614, WI#615, WI#639, WI#643).** NetBird uses a digest-pinned image and resolved-image-ID drift detection. Sidecars are instance-scoped and joined only by a verified container ID. A per-container-name registry serialises starts, recovery, stops and cancelled-start cleanup, and firewall replacement is one atomic `nft -f` transaction; a table that already verifies is not rewritten.
- **Failure handling (WI#651).** Cold starts get a 6-second adoption re-probe and the full provider budget; per-run recovery keeps a 20-second provider cap and a 60-second recovery budget. Cached failures back off for 30 seconds. A health chain has no aggregate ceiling: each probe keeps its own 10-second deadline, so a healthy chain can take up to 70 seconds on WireGuard (seven probes) and 60 seconds on NetBird (six). Errors name the failed probe and never blame a recovery that did not run.
- **Identity and DNS.** The peer identity salt is persistent, atomic, mode `0600` and reboot-stable, so a reboot no longer re-enrols the peer. The salt is published with a no-replace hard link and opened with `O_NOFOLLOW`; a damaged salt is refused with the path and reason rather than silently replaced. WireGuard accepts `SANDBOX_TUNNEL_DNS` for joined runs; NetBird refuses it and uses NetBird nameserver groups instead.
- **Teardown (WI#638).** `remove-tenant` stops and removes every lane's bridge and task containers and IPC volumes before tearing down the egress network. Teardown refuses to flush while any container is still attached, and a firewall or attachment query that cannot be completed leaves the filter and state in place for retry.

> A hull built to go<br>
> where the light gives up its claim.<br>
> It works in the dark.

## Agent and LLM runtime

- **Billed-empty retry budget (WI#578, WI#595).** `LLM_EMPTY_STREAM_RETRY_INPUT_TOKEN_BUDGET` defaults to `100000` per provider leg. The first transient billed-empty attempt is exempt and receives one transparent retry; the cumulative budget applies from the next attempt. Failover legs and engine or agent recovery re-issues start a fresh per-call budget. mt-admin preserves explicit values, including `0`.
- **Legacy V1 steer notes (WI#584).** Idle and interrupted steer notes are drained into the next turn and placed before the new request with explicit pre-turn framing, instead of being silently dropped. Timeout suppression clears pending messages and steer notes together.
- **Supervised mode is honest (WI#607, WI#617).** `--supervised` now records an explicit CLI request, resolves strictly with runtime and process-environment values, and treats any truthy source as enabled. Startup logs one record with the resolved value and its source. The foreground thread builder reads the agent's resolved configuration itself, so there is no separate supervision boolean to miswire.
- **Startup banner (WI#579).** The daemon logs `Starting LunarWing v2.2.0` at INFO from `CARGO_PKG_VERSION`.
- **Multica validation and status (WI#573).** Host-generated validation errors stay actionable through the gateway's `channel_config_errors` map, while guest and runtime startup detail remains in daemon logs and health and listings use fixed redacted text.
- **WeeChat credential precedence (WI#577).** A rotated managed `RELAY_PASSWORD` now wins over a stale per-owner store secret in channel setup and host HTTP credential injection; the store is the fallback.
- **Mission quarantine (WI#576).** An unparseable `mission.json` is quarantined at boot instead of repeatedly failing, one quarantine copy is kept per distinct content, and the warning names an allowlisted `missing_field` without logging the mission body. Partial `retry_policy` objects deserialise with defaults.
- **Study Notes staleness (WI#597).** Listed notes carry canonical `stale_reasons`, superseded notes are retired, and `include_status_only=false` lets daily routines omit status-only repeats while keeping age and source-commit matches. Submitting a note defaults `studied_at` to the submission time.
- **Turn outcome grace (WI#627).** Stop-request and post-stop observation use independent grace periods, with distinct diagnostics for a stop-request timeout and a successful stop followed by a starved wait.
- **Log scrubber scope (WI#633).** `LogScrubber::remove_secret` exists only under `cfg(any(test, feature = "test-support"))`; the shipped binary has no removal symbol, and a failed matcher rebuild keeps the previous matcher so a failed removal cannot expose a value.

## Security and supply chain

- **RSA removal (WI#368, WI#616).** RSA private keys are rejected before an identity can load, including worker mode and `ssh_git`. Persisted `ssh-rsa` pins are retained rather than silently deleted and warn at load; an RSA-only server fails during negotiation with an error naming the RSA host key and the supported alternatives. Worker-mode OpenSSH and `ssh_git`'s post-pin OpenSSH transport keep their own algorithm policies; the change applies to the shared russh client.
- **Secret-history scanning (WI#575, WI#590, WI#599, WI#618).** `secret-history` is blocking on schedules and protected release tags, requires a full checkout, and refuses shallow clones. The baseline is commit-bound, new vendor or non-vendor fingerprints require an explicit classified acceptance, and the refresh helper can draft, review and accept in one scan with redacted memoised fingerprints. The recorded replay under Gitleaks v8.30.1 scanned 11,524 commits / 2.07 GB to `no leaks found`.
- **Advisory coverage (WI#610, WI#626).** Both Pebble locks carry `rustix` `1.1.6`; the manifest-less override lock is now audited with checksum-pinned `cargo-audit 0.23.0` against the same advisory checkout, and a scratch copy carrying `rustix` `1.1.4` fails as intended with RUSTSEC-2026-0486. The sweep prints a failing root's full diagnostics instead of the tail of the output.
- **CI tool bootstrap (WI#594).** sccache and ShellCheck try the first-party generic registry before GitHub, with anonymous downloads, transient-only retries and checked-in SHA-256 enforcement on every path. A total sccache outage degrades by unsetting the compiler wrapper; integrity and other installation failures remain fatal, and ShellCheck never falls back to a distro substitute.
- **Worker policy (WI#665, WI#745).** OpenCode 2 ships a managed Console deny policy, applies managed policy precedence during the image build, and trips a build-time check when the reviewed policy text changes. OpenTunnel remains compiled out; sharing, service mode, pairing, non-loopback serve and self-update stay disabled.
- **Log and test hygiene (WI#633).** Test fixtures unregister secrets they register, and two further non-static test residues in the web gateway and setup wizard were cleaned up.

## Operations and tenant lifecycle

- **Sandbox image rebuild (WI#596).** Managed builds carry a Dockerfile and Debian-snapshot digest label. `add-tenant` and `upgrade-tenant` default to `auto`, with `always` and `never` controls. The build archives `ic/docker/` from the verified operator revision, refuses dirty, unknown or off-release sources, and distributes the same image ID to the admin and tenant stores.
- **Runtime discovery paths (WI#663).** `sandbox::runtime_path` owns one policy: append the sbin directories a `PATH` lacks, keep its order, never repeat a directory. `binary_on_path`, the podman version probe, the Docker rootless probe, the runner's five spawn sites and the reaper all resolve through it. The systemd tenant unit states `/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin` ahead of `~/.cargo/bin`; the OpenRC init script appends the missing sbin directories to the host `PATH` at start instead of replacing it, so host-specific entries such as `/opt/bin` survive. The policy is Unix-only.
- **Upgrade target (WI#708, WI#723).** `upgrade-tenant` resolves the operator-selected commit from the operator checkout, not the tenant's clone, and prefers `refs/remotes/origin/<name>` over a stale local branch. Update the checkout and fetch the intended target before upgrading.
- **Health library ownership (WI#687, WI#696, WI#703, WI#722, WI#725).** The installer stages under a root-only tree, refuses linked or non-regular sources, clears inherited modes, ACLs and set-ID bits, and leaves every file the library runs executable. The launcher validates the installed tree before loading its environment and names the failing path component. A refused source is skipped for the affected subcommand, never copied into a root-owned tree, and `reinstall-health-library <trusted-checkout>` remains available for repair on OpenRC.
- **Study Notes snapshot (WI#715).** The root-side hourly sync runs from a private mt-admin snapshot under `/usr/local/lib/lunarwing-study-notes-sync/releases/`, with a `STAGED-FROM` record. `upgrade-tenant` restages an existing scheduler; a host without one still gets none. `doctor` prints the staged commit and warns when it lags the operator checkout.
- **Root service policy (WI#574, WI#613).** One shared check guards systemd, launchd and OpenRC: a root install fails before file writes unless `LUNARWING_ALLOW_ROOT_SERVICE=1`. OpenRC also refuses a primary gid of 0 without the same opt-in, and the setup wizard does not offer an install the policy would refuse.
- **Service-manager overrides (WI#730, WI#757).** `detect_init_system` refuses an explicit `LUNARWING_SERVICE_MANAGER` that contradicts the host's PID-1 markers, and the main dispatcher runs that detection before any command body, so no handler writes first. `help` stays open and writes nothing. `LUNARWING_SERVICE_MANAGER_FORCE=1` permits the contradiction only while the ports registry holds no tenant; a registered entry counts on its own, even without its unit file, and an unreadable registry is treated as tenants present. The forcing path warns, and `health_service_manager_of_record` still records the manager the host probe reports. `LUNARWING_MT_OPENRC_MARKER` and `LUNARWING_MT_SYSTEMD_MARKER` are the documented test-only seam that repoints the probe.
- **Remove-tenant (WI#638).** Both purge modes stop every lane in the same order as `stop-tenant`, remove supported bridge and task containers and their IPC volumes, uninstall OpenRC task units and clear runlevels, and stop systemd task services and reload removed Quadlets before container deletion.
- **Cron and schedule writers (WI#740, WI#743).** Root health scheduling uses shared checked writers for the cron block and unit pair, refuses partial or unbufferable crontab listings instead of treating them as empty, and recognises an installed schedule whose cron service is merely not enabled.
- **CI and worker flake (WI#608).** The Kimi bridge installs its first-message listener before the socket can open, so the immediate `ready` frame cannot be lost, and every explicit event wait in its test has a named deadline.

## Documentation

- **Documentation sweep.** All current architecture and guide documents were revalidated, with nftables prerequisites, worker egress audits, sandbox storage and tunnel guidance, registry versions, OpenRC identity safeguards and the startup banner brought in line with the code. Archived procedures are marked historical and point at maintained replacements.
- **New operator material.** `docs/guides/SANDBOX_TUNNEL_NETWORKING.md`, `docs/proposals/PODMAN_SANDBOX_TUNNEL_NETWORKING.md`, `docs/proposals/LUNARDEPTH_WORKER_LANE.md`, the LunarDepth section of `docs/ops/WORKER-CONTAINERS.md`, and the PostgreSQL version inventory under `docs/ops/` record the new contracts.
- **Audits.** `docs/ops/v2.2.0.0_CARGO_DENY_AUDIT.md` covers all 27 tracked non-vendor lockfiles, 4,130 registry pins and the retained acceptances. The release index links this draft.

## Compatibility

| Area | v2.2.0.0 |
|---|---|
| Owned packages / core Rust | Eleven packages at `2.2.0`; Rust 2024, MSRV `1.96.1` |
| WASM runtime | Wasmtime/WASI `36.0.20`; the `wasmtime-internal-cache` patch stays on that release |
| Channel registry | darkirc `0.1.16`, multica `0.1.20`, weechat `0.2.17`, xmpp `0.1.22` |
| Tool registry | cavepony-tool `0.4.11`, gitlab `0.1.15`, gotify `0.1.7`, llm-context `0.1.9`, multica-bridge `0.1.10`, ssh `0.1.8`, vision-analyze `0.2.2`, voice-transcribe `0.1.8`, web-search `0.2.10` |
| External workers | Kimi Code `2.5.3`, OpenCode `v1.19.5`, OpenCode 2 `v2.2.7`, Pebble `2d0f3786…`, LunarDepth `0472d115…` |
| Worker toolchain | Debian snapshot `20261105T082236Z`; Node `22.23.4` (Kimi, LunarDepth); pnpm `11.7.0` (LunarDepth); Bun `1.4.4` (OpenCode lanes) |
| Sandbox | Linux default `proxy-enforced`; `tunnel` opt-in; sandbox image carries 25 pinned Python development packages |
| Terminal client / local REPL | Wingtip `0.1.0` over HTTP/SSE; daemon Unix socket supports protocol 1 and legacy compatibility responses |
| Advisory baseline | 14 accepted IDs; RUSTSEC-2023-0071 cleared, two informational chrono notices accepted (WI#749) |

Registry bumps require rebuilt WASM artifacts. Rebuild every enabled worker image through the protected candidate, scan and promotion path; local builds do not replace those gates.

## Upgrade Notes

1. **Expect the new sandbox egress default.** An unset `SANDBOX_NETWORK` on Linux, including a single-node install and a newly added mt-admin tenant, now resolves to `proxy-enforced`. Existing explicit values (`none`, `proxy-enforced`, `tunnel`, `proxy`) are preserved on re-render. Non-Linux platforms stay on `none`. Use `SANDBOX_EXTRA_DOMAINS` or `extra_allowed_domains` for additional destinations. If no exfiltration path is acceptable, keep or set `none`.
2. **Review legacy `proxy` tenants.** Only `proxy` can inject `LUNARWING_CLOUD_API_KEY` and provider keys into plain-HTTP requests. `proxy-enforced` has zero mappings. Move a tenant to `proxy-enforced` if that injection is not wanted.
3. **Prepare NetBird DNS before upgrading a tunnel tenant.** 2.2.0.0 moves NetBird sandboxes to NetBird's managed resolver and permits port 53 only to its `wt0` listener. A tenant without a primary NetBird nameserver group reachable through the overlay will resolve no public names after the upgrade; peer names and match-domain groups still resolve. WireGuard tenants can set `SANDBOX_TUNNEL_DNS` to move joined-run DNS into the tunnel. NetBird rejects that variable by design.
4. **Expect one tunnel re-enrolment on upgrade.** The identity salt now lives at `<base>/sandbox-tunnel-identity.salt`. A peer volume created before this release is not adopted by guessing; the documented upgrade performs one deliberate re-enrolment, and the old peer must be revoked.
5. **Replace RSA SSH material.** Ed25519 and ECDSA are the only accepted private-key algorithms. Configure an Ed25519 or ECDSA host key and replace `ssh-rsa` pins. Encrypted RSA host key pins are not deleted automatically; they warn at load and cannot authenticate a shared-client connection.
6. **Upgrade the operator checkout before tenant builds.** `upgrade-tenant` builds the operator-selected commit for the sandbox image and every enabled worker lane. The target must resolve in the operator checkout; for a named branch, `refs/remotes/origin/<name>` wins over a local branch. A changed sandbox Dockerfile or Debian snapshot rebuilds a candidate and keeps the old image if it fails.
7. **Refresh the health library on affected hosts.** A host installed before WI#687 can still contain checkout-owned files under `/usr/local/lib/lunarwing-health`. Updating the source checkout alone does not repair it. Run `add-tenant` or `upgrade-tenant` through the updated mt-admin, or `ic/scripts/enable-health-fleet.sh`. For an unsafe library or entry, reinstall from a separate trusted checkout:
   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh reinstall-health-library /path/to/trusted/checkout
   ```
8. **Restage or re-arm Study Notes sync.** The first `upgrade-tenant` on a host with an installed scheduler restages its private mt-admin snapshot automatically. If that host cannot upgrade yet, run `configure-study-mcp <tenant> --enable-knowledge-sync` once; until then the hourly job keeps executing the old snapshot.
9. **Reinstall or rebuild what the registries moved.** Rebuild the daemon and XMPP bridge, reinstall the four channel and nine tool extensions, and rebuild enabled worker images. `rsa`, `pkcs1` and `crypto-primes` are absent from the main lock by design; do not re-add the `russh/rsa` feature.
10. **Add the durable secret-history schedule.** The release-side job is blocking on schedules and protected tags, but a weekly schedule is a GitLab project setting, not committed YAML. Create one under **CI/CD > Schedules** so a missing pin is caught before the next release tag.

## Known Limitations

- **NetBird custom DNS is unsupported (WI#603).** Setting `SANDBOX_TUNNEL_DNS` with NetBird is refused before any container-runtime call. Use NetBird nameserver groups reachable through the overlay; an upstream outside the overlay fails closed because its resolver sockets are unmarked.
- **Tunnel live coverage is narrower than the code paths.** The gated tunnel suite was exercised on rootless Podman 5.8.2, including NetBird 0.82.0 and a real WireGuard peer. Docker fallback is harness-covered, not live-tested; no public-cloud, CI or systemd-service-mode tunnel run is claimed. Detached systemd user-manager checks report their existing skip on hosts without a user manager.
- **LunarDepth acceptance remains partly tenant-side.** The full task path was exercised against an isolated model endpoint in a disposable tenant. Real upstream inference approved by a tenant and a live systemd host remain deployment checks; the protected promotion digest gate is a CI step, not a local test claim. Local Podman image IDs differ between builds even when every rootfs entry matches.
- **Harness redaction remains bounded and best-effort.** Offset or grouped dumps, other encodings, arbitrary fragments and unmarked truncation are not comprehensively covered. The external-worker isolation boundary remains per-task-container authority; see `docs/architecture/HARNESS_WORKER_CAPABILITIES.md`.
- **Wingtip socket work is still M4.** The daemon Unix socket supports protocol 1, but Wingtip's own socket transport and reconnect suite remain future work. A missing or replaced daemon socket is logged without automatic rebind.
- **RSA policy stops at the shared client.** Worker-mode OpenSSH and `ssh_git`'s post-pin OpenSSH transport retain their own host-key algorithm policies. RSA private keys are rejected on every LunarWing SSH path; RSA host keys are unsupported only on the shared russh paths.
- **Chrono notices are accepted, not fixed.** `chrono 0.4.45` and `chrono-tz 0.10.4` are unmaintained with no patched release. No migration off chrono is planned (maintainer decision, 2026-11-17); both are informational and compiled into every feature set.
- **Some advisory acceptances cover opt-in builds only.** The libsql-remote-replica legacy chain retains its standing acceptances under WI#418; default and PostgreSQL builds exclude it by the feature gate. A green default build is not a claim that the opt-in graph is advisory-free.

> Her own light, her own.<br>
> No sun loaned her anything.<br>
> That is the whole trick.

---

> You were not sent down<br>
> to find the bottom for us.<br>
> You were sent to *be* it —<br>
> the place we can reach.

深海
