# LunarWing

## What is LunarWing

LunarWing is a self-hosted, privacy-focused AI agent runtime and operations platform written in Rust. It connects agents to open communication protocols, extensible tools, persistent memory, and operator-controlled infrastructure. Security boundaries, approvals, secret handling, health monitoring, and recoverable service operation are part of the platform rather than optional deployment afterthoughts.

<p align="center">
  <img src="docs/assets/branding/lunarwing-overview.jpg" alt="LunarWing" width="1400">
</p>

## LunarWing v2

The v2 line combines a channel-neutral execution engine, an interactive browser gateway, production multi-tenancy, persistent memory, extensible tools and channels, and guided onboarding and migration workflows.

## Quick links

- [Website](https://lunarwing.org/)
- [Blog](https://blog.lunarwing.org/)
- [Documentation index](docs/README.md)
- [Community guide](COMMUNITY.md)
- [Project manifesto](MANIFESTO.md)
- [Canonical source on GitLab](https://git.lunarwing.org/lunarwing/lunarwing2)
- [IRC: #lunarwing on Libera.Chat](https://web.libera.chat/?channel=#lunarwing)
- [AGPL-3.0-or-later license](LICENSE)

## Why LunarWing

- **Operator controlled** — run the agent, database, providers, workers, and communication bridges on infrastructure you control.
- **Open communication** — prioritize XMPP, IRC through WeeChat, DarkIRC, and other open or self-hostable paths.
- **Extensible by design** — add sandboxed WASM tools, channel adapters, and external MCP servers without folding every integration into the core daemon.
- **Secret-aware operations** — encrypted secret storage and tenant-scoped operational tooling are built into the deployment model.
- **Production multi-tenancy** — isolate tenants with separate OS users, services, databases, ports, configuration, and optional workers.
- **Reliable automation** — scheduled routines, retry and backoff, health checks, and self-healing tooling support unattended operation.
- **Lunarpunk values** — the core and bundled extensions remain free software under AGPL-3.0-or-later.

## Key v2 capabilities

### Engine V2 and the gateway

Engine V2 provides the channel-neutral thread, capability, and CodeAct execution path used by the v2 gateway. It supports streamed events, interrupts, stop handling, approval gates, and rich tool results. New multi-tenant configurations enable Engine V2 by default; direct single-instance configurations must opt in.

The gateway receives the full interactive experience. Multi-tenant deployments route XMPP, DarkIRC, and WeeChat through Engine V2 by default (`ENGINE_V2_CHANNELS=xmpp,weechat,darkirc`, with operator overrides preserved across re-renders); single-instance configurations opt in through `ENGINE_V2_CHANNELS`. Other channels remain on the legacy path. WASM channels on Engine V2 currently receive terminal responses and status updates rather than live token edits. Validate channel-specific behavior before enabling Engine V2 for additional group channels.

Architecture details: [Engine V2](docs/architecture/ENGINE-V2.md).

### Channels, tools, and workers

- Browser gateway, CLI, and HTTP interfaces
- WASM channels for XMPP, WeeChat, DarkIRC, and Multica
- XMPP file attachments via XEP-0363 HTTP File Upload
- WeeChat buffer restoration across service restarts and clean reboots, plus relay reconnect recovery: a bounded outbound queue during disconnects and post-reconnect backfill of lines accepted while the socket was down
- MCP runtimes for HTTP/HTTPS, Unix sockets, host-local stdio, persistent WASM components, separately deployed dedicated/shared gateways, explicitly selected core Docker job workers, and operator-described external runtimes
- MCP artifact pinning for host stdio/WASM, optional OS sandboxing for host stdio, bounded stdio/WASM crash recovery, and owner- or job-scoped credential grants
- MCP marketplace trust stack: ed25519 publisher signatures, signed dependency lockfiles, and signed revocation lists, enforced fail-closed on config load and activation once configured
- Optional [LunarVoice](projects/voice-sidecar/README.md) local ONNX speech processing with Whisper or Parakeet-TDT STT, Kokoro TTS, and Silero VAD
- Optional Gotify notifications, GitLab and SSH tooling, NetBird tunnels, and LunarVision OCR integration
- Persistent Pebble, OpenCode, and Kimi Code worker containers

DarkIRC is opt-in. It is not rendered or built for a new tenant unless the operator enables it and builds its supporting service. External-worker choice is stored when a tenant is created; build flags prepare the matching shared image but do not change an existing tenant's selection.

### MCP servers and sandboxed WASM runtimes

LunarWing connects to Model Context Protocol (MCP) servers through explicit operator-managed lifecycle actions (install, activate, authenticate, update, remove) available from the CLI, the browser settings UI, the authenticated API, and conversational flows.

MCP-over-WASM servers run as one persistent sandboxed component actor per server. The guest communicates over a raw JSON-RPC WIT boundary with no WASI or host imports, so it has no ambient access to the filesystem, network, or process environment. Artifacts load through a single-open path with optional operator-pinned digest verification, and execution is bounded by memory, fuel, wall-time, and message-size limits. A trapped component is rebuilt or replaced within bounded jittered retries and a circuit breaker. The transport is deliberately synchronous client-to-server tool calls only — server-initiated requests, resources, prompts, and subscriptions are not exposed — and the client negotiates protocol revision `2024-11-05`, also accepting `2025-03-26` from servers that answer it.

Host-local stdio servers get defense in depth as well: optional OS sandboxing applies resource limits, core-dump control, process-group isolation, `no_new_privileges`, seccomp denial of ptrace/process_vm, a Landlock read-only root, and network-namespace isolation. On Linux, pinned stdio servers execute verified sealed bytes atomically through `memfd` + `execveat`. Secret references stay owner- or job-scoped, and worker children receive scrubbed environments. Mandatory sandbox profiles can be required across all servers with legacy opt-in behavior preserved as the default.

An optional marketplace trust stack gates MCP packages at the checkpoint: ed25519 publisher signatures over canonical manifests, signed lockfiles pinning exact versions and digests, and signed revocation lists with monotonic sequence numbers. Once the `marketplace` section of `mcp-servers.json` is populated, revoked packages and corrupt trust stores fail closed at config load and at every activation. A bundled gateway can front MCP servers with SSE streaming passthrough and an optional loopback-only control plane, and generic external runtime descriptors can be registered via `lunarwing mcp add --transport external` with pinned provenance digests verified by the adapter.

Automatic npm/pip package installation remains disabled by design. The provenance, dependency-locking, containment, and revocation primitives now ship and are enforced when configured, but there is no signed-package marketplace install/upgrade CLI yet — catalog installation stays manual. Details and current scope: [MCP overall status](docs/ops/MCP_OVERALL_STATUS.md) and [MCP additions](docs/ops/MCP_ADDITIONS.md).

### Memory, lorebooks, and learning

LunarWing workspace memory provides durable file-like documents and hybrid full-text and vector search on PostgreSQL and libSQL. Vector search activates when an embedding provider is configured; keyword search remains available without one. See [Semantic memory search](docs/architecture/SEMANTIC-MEMORY-SEARCH.md).

The native [Study-Notes knowledge store](ic/src/workspace/README.md#study-notes-knowledge-store) gives agents provenance-aware tools to search, compare, submit, and identify stale source-verified findings. All seven canonical tools ship natively with per-note staleness thresholds and lifecycle metadata, per-agent versions remain distinct, and notes migrate with the tenant workspace. Multi-tenant hosts can auto-provision every new tenant with a read-only, always-current study-notes MCP server via `add-tenant --with-study-mcp`. The same flag also arms a one-shot first-boot import of the synced note corpus into the tenant's own private knowledge store — it never overwrites existing notes, and upgrading an existing tenant never arms it.

Bounded [lorebooks](docs/proposals/LOREBOOKS.md) load a supported subset of SillyTavern World Info JSON and inject only entries activated by the current conversation. Lorebooks are disabled by default (`LOREBOOK_ENABLED=true` opts in) because activation scanning is computationally expensive. Prompt-injection checks run when lorebooks are written and loaded, and activation in group chats is disabled by default.

Skills add reusable prompt-driven capabilities. Optional self-improvement missions can extract reusable skill documents from completed threads, while skill patches and removals remain proposals until a user approves them. Setting `SKILL_SELF_IMPROVEMENT=0` fully disables the skill-learning missions, and mission triggers are deduplicated so daemon restarts never re-fire stale work.

### Automation, coordination, and health

The optional reflex compiler detects recurring prompts and compiles them into WASM fast paths that can bypass repeated LLM work. Exact and fuzzy matching stay local; semantic matching is a separate opt-in because its embedding provider may be remote.

An opt-in [multi-agent coordination pipeline](docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md) adds tagged-message policy, secret detection, engagement and first-responder controls, rate limiting, violation-based disabling, and outbound coalescing for designated XMPP or IRC rooms. It is disabled unless explicitly configured.

Cross-tenant knowledge sharing builds on that pipeline: opted-in tenants can propagate durable `KNOW: <topic> | <finding>` findings into every watching tenant's shared study-notes scope as `needs-verification`, behind publisher allowlists and per-sender ingest budgets. It is off by default and older daemons ignore the records safely.

The included [infrastructure health system](ic-infrastructure-health-check/README.md) aggregates service and dependency checks and can remediate unhealthy services with grace periods, exponential backoff, flapping protection, and post-restart verification. Its multi-tenant service and port discovery supports both systemd and OpenRC. A Prometheus textfile exporter turns every health run into metrics — with a graduated warn/crit early-warning threshold layer and deduplicated Gotify notifications — and an optional loopback-only listener can serve the exposition behind the host-global `health_metrics` port-registry slot.

The optional LunarVision OCR and LunarVoice speech sidecars follow the same supervision contract on both init systems: health-gated starts against dedicated operational listeners, launch-contract tracking that recreates drifted containers instead of blindly restarting them, and crash-supervisor units.

### Model providers

The built-in registry supports local Ollama and OpenAI-compatible endpoints, including compatible routing gateways such as TensorZero. Operators can add or override declarative provider definitions without changing Rust code.

## Get started with the onboarding GUI

The browser-based onboarding console is the recommended setup path for LunarWing v2. It wraps the multi-tenant administration scripts while showing configuration, command output, validation results, and recovery guidance in one local interface.

Run these commands from the repository root.

### 1. Explore safely in demo mode

```bash
./lunarwing_mt_onboard_web/run.sh --demo
```

Demo mode does not create tenants or modify host services. The launcher still creates its local Python virtual environment, installs the GUI dependencies when needed, and writes local audit logs.

### 2. Prepare the production host

The supported production path assumes Python 3.10 or newer with `venv` and pip, a Linux host using systemd or OpenRC, root or sudo access, Docker or Podman, `jq`, `git`, download access on first launch, and adequate time and disk space for builds.

Review the [distribution-specific prerequisites](docs/guides/) before provisioning, then run the host doctor:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

The doctor command checks the current host, init system, container runtime, toolchain, and other provisioning dependencies. It does not verify the Python `venv` module.

### 3. Start the real onboarding console

```bash
sudo ./lunarwing_mt_onboard_web/run.sh
```

Open the token-bearing session URL printed by the launcher, normally `http://127.0.0.1:1969/?token=...`. Keep the process running until the workflow finishes, then stop it with `Ctrl+C`.

The GUI supports five operator workflows:

| Workflow | Purpose |
| --- | --- |
| Provision | Create, configure, build, start, and verify a new tenant |
| Secrets | Store or replace one named secret in the encrypted secrets store for a tenant |
| In-place upgrade | Upgrade through the current init-agnostic mt-admin lifecycle |
| Kawarimi export | Preview or create a migration bundle; Apply stops the tenant daemon and bridge |
| Kawarimi import | Inspect, stage, and restore a migration bundle on a v2 host |

Full GUI reference: [Browser onboarding console](lunarwing_mt_onboard_web/README.md).

### Review these defaults before provisioning

- The gateway binds to `127.0.0.1` by default.
- New tenants enable Engine V2, SSH support, health checks, build, and start; DarkIRC, Gotify, external workers, and optional toolchains remain disabled until selected.
- Leaving the XMPP fields blank assigns `<tenant>@xmpp.localhost` and provisions the bridge automatically.
- Container-runtime group membership is enabled by default. Membership in the rootful Docker group is effectively root-equivalent; rootless Podman normally uses per-user storage, and a `podman` group may not exist.

### Treat the GUI as a privileged local console

The onboarding server is designed for one trusted operator on loopback. It does not provide user accounts or TLS. Keep `--host 127.0.0.1` and token authentication enabled; real mode refuses non-loopback binding unless the dangerous `--allow-insecure-remote` override is supplied. Use an SSH tunnel for remote administration. Treat the session URL, browser session, generated secrets, and audit directory as credentials. Audit redaction remains defense in depth, and each audit file is forced to mode `0600`.

## Advanced setup paths

### Interactive Python CLI

The terminal onboarding client remains available for provisioning, secrets, exports, and current mt-admin in-place upgrades. It does not provide the guided Kawarimi import flow available in the GUI.

```bash
python3 -m venv .venv-mt-onboard
./.venv-mt-onboard/bin/python -m pip install rich questionary
sudo ./.venv-mt-onboard/bin/python -m lunarwing_mt_onboard
```

See the [Python onboarding CLI reference](lunarwing_mt_onboard/README.md) for resume files and non-interactive use.

### Direct multi-tenant administration

`ic/scripts/lunarwing-mt-admin.sh` remains the source of truth for operators who need exact flag-level control.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh doctor
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant ruffles --with-opencode --with-kimi
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant ruffles --with-wasm --with-opencode --with-kimi
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant ruffles
sudo ic/scripts/lunarwing-mt-admin.sh status ruffles
```

The production scripts support rootless per-user Podman or a configured rootful Docker deployment, with systemd or OpenRC service management. Choose a worker on `add-tenant`, repeat its flag on `build-tenant` to build the image, and let `start-tenant` launch the stored selection. For DarkIRC, use `--enable-darkirc` when adding the tenant, then run `sudo ic/scripts/lunarwing-mt-admin.sh build-darkirc --tenant ruffles` before starting it.

- [Multi-tenant quickstart](docs/guides/MT-ADMIN-QUICKSTART.md)
- [Production operations reference](docs/ops/MULTITENANCY-PRODUCTION.md)

## Upgrades and migration

The GUI in-place upgrade workflow wraps `lunarwing-mt-admin.sh upgrade-tenant`.
It accepts an explicit branch, tag, or commit, including four-part v2 release
tags, and uses the same systemd-user/OpenRC lifecycle as direct administration.
It applies immediately after explicit confirmation; PostgreSQL backup and unit
rendering are enabled unless deliberately disabled.

For a v1 tenant moving to v2, prepare a separate v2 deployment and rehearse a staged Kawarimi export/import. Validate the restored tenant before cutover. Before selecting Start, the operator must stop the old tenant daemon and bridge when both tenants use the same XMPP identity; the GUI does not enforce this gate.

GUI import defaults to `apply=false` and `start=false`: it validates the real bundle and prints a dry-run plan. Apply restores and stages the tenant; Start is a separate opt-in. These safe defaults are GUI-specific—the direct import script mutates the host unless `--dry-run` is passed explicitly.

Kawarimi supports PostgreSQL tenants; libSQL bundles are refused. A real export validates its passphrase before stopping tenant writers and produces a root-owned mode-`0600`, AES-256 encrypted `.7z` bundle containing the master key and XMPP credentials. Encrypted import requires the passphrase even for dry-run. Import creates fresh host-local gateway and webhook tokens, so clients must authenticate again. Back up first, review the plan, protect bundles in transit, and delete them after the restored tenant is verified.

- [Machine migration runbook](docs/ops/MT-MACHINE-MIGRATION.md)
- [Legacy upgrade notes](docs/ops/MT-LEGACY-UPGRADE-NOTES.md)

## Development and testing

The Rust workspace lives under `ic/` and uses Rust 1.96.1 with edition 2024. On a resource-constrained development host, limit every Cargo command to twelve CPUs and twelve jobs, and prefer `cargo check` over a full debug build:

```bash
cd ic
taskset -c 0-11 cargo fmt --all -- --check
taskset -c 0-11 cargo check -j12
taskset -c 0-11 cargo test -j12 -- --test-threads=12
```

Additional test surfaces:

- [Rust and browser E2E tests](ic/tests/e2e/README.md)
- [External worker harness](tests/README.md)
- [Testing guide](docs/guides/TESTING_GUIDE.md)
- [XMPP integration harness](ic/testing/lunarwing-xmpp/README.md)

## Documentation

| Area | Reference |
| --- | --- |
| Documentation index | [docs/README.md](docs/README.md) |
| Architecture | [docs/architecture/](docs/architecture/) |
| Operator guides | [docs/guides/](docs/guides/) |
| Production operations | [docs/ops/](docs/ops/) |
| Release notes | [docs/releases/](docs/releases/) |
| Known bugs and status | [docs/bugs/README.md](docs/bugs/README.md) |
| Project blog | [https://blog.lunarwing.org/](https://blog.lunarwing.org/) |
| Canonical source (GitLab) | [https://git.lunarwing.org/lunarwing/lunarwing2](https://git.lunarwing.org/lunarwing/lunarwing2) |

## Project and community

LunarWing focuses on user freedom, self-hostable infrastructure, and open communication protocols. Read the [manifesto](MANIFESTO.md) for the project philosophy and the [community guide](COMMUNITY.md) for participation details.

[![Chat on IRC](https://img.shields.io/badge/IRC-%23lunarwing-00b0aa?style=for-the-badge&labelColor=000000)](https://web.libera.chat/?channel=#lunarwing)

LunarWing is self-funded and maintained by volunteers. The project is licensed under [AGPL-3.0-or-later](LICENSE).
