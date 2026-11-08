# Release Notes for LunarWing v2.1.9.0

## Codename: Kaitei (海底, かいてい) — The Seabed

**Release Date:** 2026-11-08

> Below the last wave,<br>
> old currents settle in sand;<br>
> stone holds what we build. 海底

*The surface moves. The seabed carries it.*

Umioto listened to the sea. **Kaitei strengthens the water beneath it.** This release concentrates on reliable model responses, trusted worker builds, deeper credential redaction, and quieter, more useful host monitoring. The daemon's local REPL gains a versioned socket foundation while service installation and dependency maintenance reinforce the layers below.

## Highlights

- **Response caching keeps the right answers (WI#545, WI#572).** Keys include canonical request metadata and a model identity known before dispatch; smart routing and failover bypass the outer cache. Empty, reasoning-only and corruption-flagged replies are excluded, as are tool-call streams. Stream hits preserve the original finish reason without replaying discarded retry usage.
- **Engine V2 recovers malformed final answers (WI#571, WI#585).** Uppercase `FINAL…</arg_value>` pseudo-actions are normalized before lease enforcement, preserving the supplied answer instead of looping on `No lease for action`. Ordinary action names such as `finalize_order` keep their structured parameters.
- **Harness secret handling follows more output shapes (WI#383, WI#580).** Per-job multi-pattern matching covers bounded raw and encoded variants, decoded credential fields and URL passwords, numbered-line views, and explicitly marked truncated tool output. Public credential metadata stays usable. Failed dispatch preparation cleans up partial harness directories, and write-back promotion has a five-minute deadline that `wait=true` respects.
- **Tenant worker builds use operator-controlled source (WI#539, WI#540).** All four lanes validate the tenant revision against the operator's selected release ref and build from a private operator-owned Git archive. Unknown, off-ref, dirty worker sources and unsafe archive entries are refused before building. Git probes run with the checkout owner's identity and constrained trust settings.
- **The local REPL gets protocol-1 hardening (WI#458).** Shared wire types, a mode-0600 socket, same-UID peer checks, strict parent-directory ownership, stale-socket recovery and bounded I/O strengthen the existing `lunarwing repl` path. Sandbox mount protection covers the socket's parent directory. This is the daemon-side M3 foundation; Wingtip's own socket transport remains M4 work.
- **Health monitoring reaches the filesystem (WI#563, WI#586).** Host and tenant stores gain block/inode capacity checks, per-filesystem alerts and Prometheus gauges. Disk monitoring never deletes data or restarts services. The host health pipeline now defaults to 15 minutes on both OpenRC and systemd; separate systemd container healthchecks default to a configurable 60 seconds to reduce probe overhead.
- **Generated services start more predictably (WI#567, WI#568, WI#569).** Systemd-user units load the optional bootstrap environment file and start with `--no-onboard run`. OpenRC installation refuses a root daemon identity unless explicitly opted in, handles blank identity hints, and fails when the primary group cannot be resolved.
- **Multica configuration errors name the missing field (WI#314).** Null, absent and blank `multica_url` or `workspace_id` values produce specific startup errors without echoing configuration values. The channel registry advances to `0.1.19`.
- **Worker versions and failure reporting advance (WI#543, WI#582).** Kimi Code moves to `2.5.2` and OpenCode 2 to `v2.2.6`, retaining the exposure guards and bounded shutdown contracts. Unknown worker statuses now produce bounded diagnostics and map to failure on both streaming and cancellation-handoff paths.
- **Dependency and secret scanning dig deeper (WI#565, WI#566).** All eleven owned release packages move to `2.1.9`; compatible dependency updates, lockfiles and vendored sources are refreshed. The dated audit covers all 27 tracked lockfiles with no new advisory acceptances. Current-tree secret scanning adds a dedicated vendor pass, and history scanning now also runs on protected release tags.

See the [v2.1.8.0 release notes](docs/releases/RELEASE-v2.1.8.0.md) for the previous compatibility baseline and the [v2.1.9.0 goals](docs/ops/AGENT_GOALS_2.1.9.0.md) for implementation evidence.

## Compatibility

| Area | v2.1.9.0 |
|---|---|
| Owned packages / core Rust | Eleven packages at `2.1.9`; core Rust 2024, MSRV `1.96.1` |
| WASM runtime | Wasmtime/WASI `36.0.20`, Cranelift `0.123.20` |
| Channel registry | darkirc `0.1.15`, multica `0.1.19`, weechat `0.2.16`, xmpp `0.1.21` |
| Tool registry | cavepony-tool `0.4.10`, gitlab `0.1.14`, gotify `0.1.6`, llm-context `0.1.8`, multica-bridge `0.1.9`, ssh `0.1.7`, vision-analyze `0.2.1`, voice-transcribe `0.1.7`, web-search `0.2.9` |
| External workers | Kimi Code `2.5.2`, OpenCode `v1.19.5`, OpenCode 2 `v2.2.6`, Pebble ref `2d0f3786…` |
| Worker image toolchain | Debian snapshot `20261105T082236Z`; Node `22.23.4` (Kimi); Bun `1.4.4` (OpenCode lanes) |
| Terminal client / local REPL | Wingtip `0.1.0` over HTTP/SSE; daemon Unix socket supports protocol 1 and legacy compatibility responses |

Registry bumps require rebuilt WASM artifacts. The [November 7 cargo-deny audit](docs/ops/v2.1.9.0_CARGO_DENY_AUDIT.md) records the dependency baseline, including 13 retained advisory acceptances and an informational `astral-tl` maintenance notice.

## Upgrade Notes

1. **Back up and deploy the components together.** Preserve the database, tenant state, workspaces, port registry and channel state. Rebuild the daemon and XMPP bridge, reinstall the four channel and nine tool extensions, and rebuild enabled worker images through the normal candidate, scan and promotion path. Use mt-admin's tenant upgrade/restart flow to activate the new images.
2. **Advance the operator checkout before tenant builds.** Default `build-tenant` and `build-all` validate against its current local branch, or exact HEAD when detached. Fetching a remote ref alone does not advance that branch. Tenant worker sources must be clean and their commit must exist in the operator repository on the selected release history; `upgrade-tenant --target` can select an explicit ref. See the [worker runbook](docs/ops/WORKER-CONTAINERS.md#rebuilt-image-rollout-image-id-contract).
3. **Review local service and socket configuration.** Regenerate single-node service units to pick up the environment and non-interactive startup fixes. Prefer a non-root OpenRC service identity; intentional root installs require `LUNARWING_ALLOW_ROOT_SERVICE=1`. Put a custom `LUNARWING_SOCKET` in a dedicated daemon-owned directory that is not group- or other-writable: its entire parent is protected from sandbox mounts. See the [socket setup guidance](docs/guides/WINGTIP_GUIDE.md#protocol-1-unix-socket).
4. **Review both health cadences.** Persist `LUNARWING_MT_HEALTH_INTERVAL_MIN` (1–59, default 15) and, on systemd hosts, `LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC` (10–3600, default 60) in `/etc/lunarwing/health.env` as plain, unquoted `NAME=value` lines without inline comments. One-off environment overrides take precedence. Longer container intervals delay failure detection; updated units/containers must be activated. The [deployment wiring guide](docs/architecture/SELF_HEAL_DEPLOYMENT_WIRING.md) explains rollout, and the [disk runbook](docs/ops/DISK_HEALTH_RUNBOOK.md) covers thresholds and manual cleanup.
5. **Check Multica and cache expectations.** Complete the required Multica fields before restarting the channel. With response caching enabled, dynamically routed/failover requests and unusable output now miss or bypass the cache intentionally; do not treat reduced hit counts as a reason to weaken those checks.

> Silt drifts past the hull,<br>
> anchors find the hidden ground;<br>
> the dark holds us fast.

## Known Limitations

- **Harness redaction remains bounded, best-effort handling.** Offset/grouped dumps, other encodings, arbitrary fragments and unmarked cuts are not comprehensively covered. The required runtime-equipped lane for combined secrets/write-back coverage remains outstanding; see the [capability boundaries](docs/architecture/HARNESS_WORKER_CAPABILITIES.md#explicitly-out-of-scope-follow-ups). External-worker isolation retains its existing per-task-container authority boundary.
- **Socket and channel follow-ups remain.** Wingtip's socket transport/reconnect suite belongs to M4. A missing or replaced daemon socket is logged without automatic rebind. Multica's host-side pre-start validation and accurate external startup-status reporting remain separate WI#314 work.
- **Secret-scan coverage is explicit (WI#575).** The vendor pass disables the generic API-key, PKCS#12 and private-key rules and pins remaining known fixtures by exact fingerprint. Historical scanning remains observational while findings are triaged and credentials remediated; a green current-tree scan is not a clean-history claim. See the [CI scanning policy](.gitlab/README.md).

---

> Under quiet tides,<br>
> small repairs become bedrock;<br>
> we rise from the deep.

海底
