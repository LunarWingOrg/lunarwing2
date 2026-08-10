# Release Notes for LunarWing v2.0.6.0: Codename `Kairo (回路)`

**Release Date:** 2026-08-09

> *Kairo* (回路) means "circuit." LunarWing v2.0.6.0 connects more execution paths
> through bounded MCP runtimes, native shared knowledge, local voice processing, and
> Engine V2 channel routing.

## Overview

LunarWing v2.0.6.0 is an extension-runtime, local-AI, knowledge, and reliability
release built on v2.0.5.0. It completes the planned MCP Part 2 runtime slices,
adds a native Study Notes knowledge layer, introduces the opt-in LunarVoice ONNX
sidecar, and routes XMPP, WeeChat, and DarkIRC through Engine V2 by default for
new multi-tenant configuration while preserving operator overrides.

The release and tag use `2.0.6.0`; LunarWing-owned Rust crates and channel
packages use `2.0.6`. Rust 2024 and MSRV `1.96.1` remain unchanged. Database
migration `V26` adds a study-note metadata index for PostgreSQL, with matching
libSQL migration behavior. The multi-tenant port registry advances to v15 for
isolated LunarVoice service and health ports.

See the [v2.0.5.0 release notes](docs/releases/RELEASE-v2.0.5.0.md) for the
previous compatibility baseline.

## Highlights

- Completed MCP Part 2 runtime support: a common adapter boundary, stateful
  MCP-over-WASM, dedicated/shared gateway placement, job-worker placement,
  worker restart supervision, package provenance pins, and opt-in host sandboxing.
- Added five native `knowledge_*` tools backed by ordinary workspace documents,
  with private/shared scopes, provenance, lifecycle status, stale-note sweeps,
  PostgreSQL/libSQL search, and Kawarimi migration coverage.
- Added LunarVoice for bounded local speech-to-text, voice activity detection,
  and text-to-speech. STT supports Whisper and NVIDIA Parakeet-TDT
  `0.6b-v2-int8`/`0.6b-v3-int8` through sherpa-onnx.
- Enabled Engine V2 routing for XMPP, WeeChat, and DarkIRC in generated
  multi-tenant configuration; existing values and explicit opt-outs survive
  rebuilds and re-renders.
- Added bounded Engine V2 model-failure recovery and stopped mission goals from
  polluting normal conversation-insight extraction.
- Disabled lorebooks by default; deployments that use them must now set
  `LOREBOOK_ENABLED=true`.
- Reduced false-positive secret-warning noise, fixed Multica API allowlisting
  under base-path deployments, and improved WeeChat/OpenRC and DarkIRC
  multi-tenant operations.

## Compatibility

| Area | v2.0.6.0 behavior |
|---|---|
| Release version | `2.0.6.0` |
| Crate/package version | `2.0.6` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | `V26__study_notes_knowledge.sql`; automatic on startup |
| Study Notes backends | PostgreSQL and libSQL |
| Multi-tenant Engine V2 channels | `xmpp,weechat,darkirc` by default; existing overrides preserved |
| Lorebooks | Disabled by default; `LOREBOOK_ENABLED=true` opts in |
| LunarVoice | Disabled until explicitly enabled and verified models are installed |
| MCP host sandbox | Opt-in per host-local stdio server |
| MCP worker placement | Core Docker jobs, stdio servers, preinstalled commands only |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### MCP Runtime and Isolation

- Added a public runtime-adapter registry that dispatches HTTP, stdio, Unix,
  WASM, and gateway-backed MCP servers through one validated factory path.
- MCP-over-WASM runs a persistent JSON-RPC component actor without WASI or host
  imports and enforces digest, memory, fuel, epoch, message-size, and response
  validation bounds.
- Dedicated and shared gateway modes now carry host-owned placement and routing
  headers. LunarWing supplies the client contract; operators still deploy the
  gateway control-plane service separately.
- Host-local stdio servers can opt into OS sandboxing. Portable controls cover
  process groups and resource limits; Linux additionally supports
  `no_new_privileges`, syscall restrictions, Landlock, and network isolation.
- Operator-pinned package identity, executable/component digests, trust state,
  revocation, and CLI management now protect activation from artifact drift.
- Core podman jobs can activate owner-approved stdio MCP servers from immutable
  snapshots. Job-scoped credential grants, worker-image provenance checks,
  bounded discovery, atomic registration, and fail-closed startup/cleanup keep
  the server inside the job boundary.
- Worker-local servers now restart after unexpected post-start failure with
  bounded backoff, a stability window, and a five-attempt circuit breaker.
- OAuth-gated activation remains enabled while authentication completes, so the
  callback can activate the server and resume the waiting Engine V2 thread.

### Native Study Notes

- Added `knowledge_search`, `knowledge_get_topic`, `knowledge_submit_note`,
  `knowledge_list_stale`, and `knowledge_stats` as protected built-in tools.
- Notes are versioned workspace documents under `study-notes/<agent>/<topic>.md`,
  searchable across private and shared scopes with source, agent, submitter,
  session, status, tag, confidence, and commit provenance.
- A background sweep marks aging active notes stale without blocking heartbeat
  work. Threshold, cadence, and shared-scope inclusion are configurable.
- Study notes travel with normal tenant database export/import and owner-scope
  rekeying; the external Study Notes MCP remains optional rather than required.

### LunarVoice

- Added a bounded Rust ONNX sidecar with bearer-authenticated loopback APIs,
  separate health/readiness/metrics endpoints, concurrency/rate/body/time limits,
  and fixed-argument media decoding.
- Added daemon transcription-provider integration and a sandboxed WASM
  `voice-transcribe` tool.
- Supports Whisper Tiny plus Parakeet-TDT v2 int8 (English) and v3 int8
  (multilingual), with engine-aware model layouts and readiness checks.
- Added systemd-user/Quadlet and OpenRC lifecycle support, per-tenant port
  allocation, registry-aware health checks, enable/disable commands, and
  SELinux-aware rootless Podman startup.
- Models are not committed or silently trusted. Enabling LunarVoice requires
  locally installed artifacts and SHA-256 verification markers.

### Engine, Channels, and Operations

- Generated multi-tenant environments now default to `ENGINE_V2=true` and
  `ENGINE_V2_CHANNELS=xmpp,weechat,darkirc`; an existing value, `false`, or
  `none` remains durable across provisioning operations.
- Engine V2 classifies final provider failures, retries recoverable failures once
  with bounded safe guidance, compacts context-overflow history, and avoids
  retrying authentication or rejected-request failures.
- Conversation insights exclude mission-thread goals before applying their
  five-goal limit.
- Ambiguous standalone 64-character hex values remain detected and masked, but
  the default heuristic logs them at `DEBUG` instead of `WARN`; block/redact and
  custom warning policies are unchanged.
- Multica/Lunartica capability allowlists can match an API subtree beneath an arbitrary
  deployment base path without allowing traversal.
- Multi-tenant WeeChat capability files receive allocated relay/adapter ports;
  OpenRC tracks the real tmux process and reports live sessions correctly.
- DarkIRC builds now work from fresh hosts with the required build-directory
  traversal, embedded revision, artifact ownership, and hard-link handling.
- Compatible dependency updates and package-version bumps are included, but the
  final release-wide lockfile reconciliation remains a pre-tag gate.

## Upgrade Notes

1. Back up PostgreSQL, tenant env files, `/etc/lunarwing/ports.json`, workspace
   state, rendered units, worker data, voice models, and channel state.
2. Allow startup migration V26 and the v15 port-registry migration to finish
   before mixed-version writes or LunarVoice activation.
3. Re-render normal multi-tenant configuration, then review
   `ENGINE_V2_CHANNELS`. Existing overrides are preserved; use `none` or
   `ENGINE_V2=false` for a durable opt-out.
4. Set `LOREBOOK_ENABLED=true` only on deployments that intentionally accept its
   activation cost and content policy.
5. Keep LunarVoice disabled until model artifacts and SHA-256 markers are
   installed. Review `VOICE_STT_ENGINE`, model paths, language, and loopback
   bearer-token configuration before enabling it.
6. Review every MCP placement and provenance pin. Host sandboxing is not a
   mandatory default, and worker placement accepts only preinstalled stdio
   commands in supported core Docker jobs.
7. Reinstall/re-render WeeChat and health assets so tenant ports, OpenRC status,
   and LunarVoice discovery match the current registry.

## Known Issues

### Security, Runtime, and Release Limitations

- DarkIRC Secure Key Exchange Stages B-D and automated QR/web/trusted-channel delivery remain
  disabled; Stage A live-host release acceptance remains a release/deployment
  gate rather than proof of activated contact security.
- MCP does not automatically install npm/pip packages. Publisher signatures,
  dependency locks, marketplace revocation distribution, mandatory sandbox
  profiles, persistent external-worker placement, and a bundled gateway
  control-plane service remain deferred. Custom MCP HTTP header values are still
  stored directly rather than through owner-scoped secret references.
- The web UI still places its bearer token in native `EventSource` query strings.
  Keep access/proxy logs protected until short-lived SSE tickets or cookie-scoped
  sessions replace this transport.
- Onboarding remains a trusted local console without user accounts or TLS. Keep
  it loopback-bound with token authentication and use an SSH tunnel remotely.
- OpenRC has portable process-tree cleanup but not systemd-equivalent hard
  memory, CPU, and task ceilings.
- `ALLOW_PRIVATE_IPS=1` weakens the public-address attachment boundary; use it
  only for trusted private networks.
- Engine output safety still truncates oversized prefixes before later
  redaction/policy transforms and does not re-cap every later transformation.
- Kawarimi remains PostgreSQL-only
- Current GitLab CI verifies source but does not publish release artifacts,
  packages, or tenant deployments.
