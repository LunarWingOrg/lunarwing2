# Release Notes for LunarWing v2.1.7.0

## Codename: Shiranui (不知火)

**Release date:** 2026-10-30

> Lights on dark water,<br>
> no fire, yet the way opens,<br>
> we steer by their glow. 不知火

*Lights on the water, with no flame behind them.*

Tsuno reached beyond the old outline. **Shiranui makes the passage trustworthy.** This release puts the external-worker lanes behind a real bridge/task boundary, gives Engine V2 explicit thread ownership, and closes the operator lifecycle gaps that could leave a tenant stranded. Beneath the visible surface, the dependency and image supply chain is refreshed and the tenant lifecycle controls are made fail-fast.

These are lights to steer by: mostly invisible from the shore, but essential before crossing into what comes next.

## Highlights

- **External workers move to non-root bridge/task pairs (WI#454).** Kimi, OpenCode, OpenCode 2, and Pebble now use two non-root containers per lane: a bridge identity (`10101`–`10104`) that holds the hub credential, and a task identity (`10001`–`10004`) that runs agent code. Both roles drop all capabilities and run with `no-new-privileges`; the task container gets neither the bridge home nor its PID namespace. The pair shares a role-owned IPC-v2 volume with sticky, role-specific directories. Paired systemd Quadlets and OpenRC units, listener/egress/health audits for both roles, task-container stop and export quiescence, pinned workspace mounts, and workspace-identity checks are part of the contract. The local Compose examples and worker documentation are migrated with it. WI#425's out-of-band image verification and Pebble hardening close alongside the split.
- **Worker credentials and tool boundaries stay fail-closed (WI#373, WI#425, WI#474, WI#495).** Only the bridge container receives `AGENT_AUTH_TOKEN`; it stages the token in a private `0400` file under its own home and exports only the file pointer to the bridge. The task container has neither that home nor the bridge PID namespace. OpenCode and OpenCode 2 reject non-empty `context.environment` before delegation; the hub refuses non-empty `create_job.credentials` for workers configured with `supports_credentials = false`, before secret lookup or dispatch, and mt-admin writes that flag for managed entries. Kimi and Pebble scrub the reserved hub and tool-proxy keys after every environment merge. BuildKit `v0.34.1` and its parser module pin the worker build, and token generation requires valid OS randomness.
- **WASM HTTP boundaries are tighter (WI#429, WI#430, WI#432).** Tools and channels share private-target and credential-redaction guards. Loopback exemptions are exact-origin and channel-specific, direct requests pin vetted DNS answers, encoded and cross-field credential echoes are scrubbed, and ambiguous transfer-coding shapes are refused. Empty-body HEAD, 204, and 304 responses no longer fail the opaque-coding guard.
- **Egress becomes a guarded lifecycle (WI#424, WI#504, WI#516, WI#511).** mt-admin provisions per-tenant networks and nftables allowlists with static host mappings and recursive DNS disabled, keeps the rootless network-namespace keeper alive across apply and verify, and fails closed if the filter cannot be proven live. Host health repeats the read-only audit, and bridge and task containers are audited separately. The keeper now prefers the running Podman pause tag and pins the local `--infra-image`; IP-only allowlists no longer make launch-status generation report failure after a successful start. OpenCode 2's runtime smoke runs in its own network namespace, so host listeners cannot trip the guard.
- **Engine V2 adopts explicit thread ownership (WI#502) and closes its gate/history edges (WI#442, WI#472, WI#473, WI#481).** A caller-supplied `preferred_thread_id` is refused when an existing record belongs to another user, both at engine spawn and at the hybrid store's keyed write; same-owner adoption and the legacy-owner stamp still work. Thread-less approvals resolve their owning gates server-side, terminal turns reap turn-bound gates, expired gateway-gate replies reach the owning thread, and a bounded one-time import brings eligible v1 history into newly created Engine V2 surfaces without backfilling existing surfaces or reviving `/clear`.
- **Model recovery is durable and visible (WI#478).** Typed recovery lifecycle events, durable turn-history correlation, and additive chat/SSE fields make retries, exhausted attempts, and terminal failures readable in history without implying that a retry is terminal. Rejected delegation drafts steer toward structured `create_job`, and recovery survives gate resumes and stale-session history fallback.
- **Mission and turn lifecycle reports honestly (WI#321, WI#323, WI#394).** Duplicate default projects resolve deterministically to the oldest `(created_at, id)` record, dropped boot records produce bounded warnings, and the mission scheduler emits start, heartbeat, fire, overdue, and auto-completion signals without user payloads. Boot recovery heals a settled-stop dedup window without replaying the lost run.
- **External-worker progress says what it is doing (WI#386, WI#475, WI#476, WI#477).** Request waits, elapsed time, retry notices, and per-attempt LLM deadlines are visible at the job boundary. Heartbeats update a separate liveness stamp, so a slow LLM is not mistaken for a dead worker; threshold breaches leave the job `Stuck`, emit one manual-intervention alert per job per boot, and expose `stuck` and `repair_attempts` in job details. OpenCode retry diagnostics can no longer masquerade as successful output. External-job cancellation synchronizes the shared context before persisting `cancelled`, supports idempotent cancellation, and accepts `Stuck -> Cancelled`.
- **Tenant operations fail fast and leave less behind (WI#486, WI#505, WI#509, WI#513).** OpenRC worker stops no longer deregister the `-sup` init script, so `restart-tenant` can recover from a failed lane start. Lifecycle and upgrade-lock descriptors are tracked and scrubbed from `rc-service` and `systemctl --user` children; a held lock fails fast naming live inheritor pids instead of blocking forever. A generalized port-drift preflight checks environment files, both init systems' proxy units, DarkIRC configuration, and Quadlet `PublishPort`s, refusing non-loopback listeners and host-less publishes. PostgreSQL root-store probes use typed `container inspect` and refuse to start when the data volume is absent; Podman volume matching no longer cross-matches by prefix. The opencode2 builder floor is checked before a tenant stop, and worker starters assert unallocated-port skips before any runtime or unit call.
- **Jobs, tools, and workspace surfaces improve (WI#315, WI#379, WI#441, WI#471, WI#479, WI#493, WI#525).** The Jobs table shows mode, and `GET /api/jobs` reports lane attribution with case-insensitive filtering. GitLab blob search accepts branch, tag, or SHA refs; `list_jobs` deserializes typed page summaries and clamps pages to 50 so failing pipelines no longer materialize the full job tree. Workspace writes reject control-bearing paths, Pebble rejects malformed pasted configuration, mount protection recognizes structural tenant layouts, and the inline v8 port migration backs up existing registries and refuses collisions or parse errors before replacement.
- **Redaction and completion backstops improve (WI#355, WI#383, WI#455, WI#491, WI#492).** Secret-safe truncation walks token boundaries and rescans retained text. Harness write-back scrubs staged paths and promotion summaries, refuses duplicate completion side effects, and reports a dedicated Finalizing refusal. The engine reservation now covers the full `engine/**` tree on memory write lanes, including `engine/.runtime/**`, while reads remain searchable. `secret_safe_cut`'s second-window boundary is recorded as an accepted limitation.
- **Supply chain and CI move together (WI#488, WI#524).** All eleven LunarWing-owned release packages move to `2.1.7`; registry packages, external crates, lockfiles, and vendored sources advance together. Wasmtime moves to `36.0.20` (Cranelift `0.123.20`) for RUSTSEC-2026-0424, and its local cache patch selects the fixed `zstd 0.14.3` chain; Pebble's source and image-override locks pin `pulldown-cmark 0.13.5` for RUSTSEC-2026-0414; internal licenses were normalized to AGPL while preserving Cavepony's upstream MIT terms; the unpinned `tui/node_modules` tree was removed. Worker images move to the `20261024T011300Z` Debian snapshot with Node `22.23.4` for Kimi and Bun `1.4.4` for both OpenCode lanes, including the Brotli decoder fix and the photon-node patch fixer. The four protected candidate jobs publish reproducible digests on master pipeline 6768.

See the [v2.1.6.0 release notes](docs/releases/RELEASE-v2.1.6.0.md) for the previous compatibility baseline. Historical notes remain a record of that release, not a current work-item status list.

## Compatibility

| Area | v2.1.7.0 |
|---|---|
| Owned release packages | Eleven packages at `2.1.7` |
| Rust / database / ports | Rust 2024, MSRV `1.96.1`; SQL migrations remain at V28; multi-tenant port registry remains v17 |
| WASM runtime | Wasmtime/WASI `36.0.20`, Cranelift `0.123.20`; the local cache patch pins `zstd 0.14.3` |
| Channel registry | darkirc `0.1.13`, multica `0.1.16`, weechat `0.2.14`, xmpp `0.1.19` |
| Tool registry | cavepony-tool `0.4.8`, gitlab `0.1.12`, gotify `0.1.4`, llm-context `0.1.6`, multica-bridge `0.1.7`, ssh `0.1.5`, vision-analyze `0.1.7`, voice-transcribe `0.1.5`, web-search `0.2.7` |
| External workers | Kimi Code `2.3.0`, OpenCode `v1.18.25`, OpenCode 2 `v2.1.2`, Pebble ref `2d0f3786…` with override lock digest `19f4e96c…` |
| Worker isolation | Two non-root containers per lane; task uids `10001`–`10004`, bridge uids `10101`–`10104`; IPC-v2 volume |
| Worker image toolchain | Debian snapshot `20261024T011300Z`; Node `22.23.4` (Kimi); Bun `1.4.4` (OpenCode and OpenCode 2) |
| Worker build policy | BuildKit `v0.34.1-rootless` and matching `v0.34.1` Dockerfile parser module; Go `1.26.10` guard toolchain |

The registry bumps require rebuilt WASM artifacts; they do not by themselves change channel protocols. Pebble's image-override lock digest is `19f4e96c…`, independently of its unchanged upstream source ref.

## Upgrade Notes

1. **Back up and deploy components together.** Preserve the database, tenant state, port registry, workspaces, and channel state. Rebuild and restart the daemon and XMPP bridge, then rebuild/reinstall all four channel and nine tool registry extensions. No SQL or port-registry migration is introduced by this release.
2. **Rebuild and recreate every worker image pair.** The split worker contract is an image, mt-admin, unit, and IPC-volume change. Rebuild through the normal candidate → scan → promotion path, then run `upgrade-tenant` or `build-tenant --with-*` followed by `restart-tenant`; old single-container images are not retroactively split. The new volume is `lunarwing-<lane>-<tenant>-ipc-v2`; do not rename or reuse the old volume. Preserve the workspace. The rebuilt images carry the Debian snapshot, Node/Bun security pins, Brotli decoder fix, and the photon-node patch fixer. The protected candidate path publishes reproducible digests for all four lanes.
3. **Review egress before restarting tenants.** Use [mt-admin's worker runbook](docs/ops/WORKER-CONTAINERS.md) and check `worker-egress <tenant>` after rollout; old running containers are not retroactively filtered by a source update. Pebble's backend needs an explicit `configure-worker-egress <tenant> --allow <backend-host>:443` grant and a restart for static host mappings (WI#506). Do not bypass the fail-closed startup errors.
4. **Review credentials and idle thresholds.** OpenCode and OpenCode 2 jobs must omit `credentials` and non-empty `context.environment`; managed entries gain `supports_credentials = false`, while unmanaged or renamed entries need that setting explicitly, followed by a daemon restart. `AGENT_EXTERNAL_STUCK_THRESHOLD_SECS` defaults to 1800 seconds, separately from the 300-second local threshold; it is an idle detector, not an LLM deadline or automatic task restart.
5. **Expect bounded Engine V2 history import and thread ownership.** The first message on a newly created Engine V2 surface may import eligible pre-switch history once. Existing surfaces are not backfilled; failed imports are best-effort. Engine V2 routing remains opt-in for XMPP, DarkIRC, and WeeChat, not a replacement of every v1 channel loop. A forged or foreign `preferred_thread_id` is refused at engine spawn and at the hybrid store's keyed write.
6. **Review the dated security evidence.** The [v2.1.7.0 cargo-deny audit](docs/ops/v2.1.7.0_CARGO_DENY_AUDIT.md) records the post-sweep advisory, ban, license, and source results, including the October 28 Pebble fix. Accepted advisories and graph exclusions still apply.

> Green light feeds the swarm,<br>
> plankton drift like scattered stars;<br>
> the whole sea breathes here.<br>

## Known Limitations

- **Worker isolation has a documented boundary.** The split separates bridge credentials from task code, but every job inside one task container remains one authority domain. The task uid owns the runner and its socket, so task code can impersonate the runner or deliberately escape its process group. The bridge still receives the lane's LLM environment, and runtime/override environment files are not independently config-hashed; force-recreate the pair after changing them. Rootful fixed IDs provide less host-escape separation than per-tenant mappings; rootless keep-id is unchanged.
- **Egress is explicit for Pebble.** A `--with-pebble` tenant has no automatic allowlist entry for Pebble's model backend. Grant it with `configure-worker-egress <tenant> --allow <backend-host>:443` and restart. Old running containers are not retroactively filtered by a source update. The rootless keeper and local infra-image pin are hardened (WI#504/WI#516).

> Dusk sinks into blue. <br>
> Lanternfish climb to the moon, <br>
> the largest journey. <br>

The [v2.1.6.0 security and deployment boundaries](docs/releases/RELEASE-v2.1.6.0.md#security-deployment-and-release-limitations) apply where not superseded above. Worker tool grants remain empty by default and must be assigned deliberately.

---

> Beyond charted lines,<br>
> small lights show the open sea,<br>
> we sail into dawn.
