# LunarWing

> Claws are overrated. Grow your wings and fly.

> **⚠️ Repository migration notice (2026-07-23):**
>
> LunarWing has left Codeberg. The decision follows recently announced
> changes to the Codeberg Terms of Service that we find incompatible with
> the project's values, combined with persistent infrastructure neglect —
> significant downtime throughout the past month with no visible improvement
> or communication from the platform.
>
> The canonical repository is now hosted on our own GitLab instance at
> [git.lunarwing.org](https://git.lunarwing.org/lunarwing/lunarwing2).
> The Codeberg repository is frozen and will not receive new pushes.
>
> Please open issues, submit merge requests, and clone from the new
> canonical location going forward.

LunarWing is a self-hosted, privacy-focused AI agent runtime and operations platform written in Rust. It connects agents to open communication protocols, extensible WASM tools, and operator-controlled infrastructure.

<p align="center">
  <img src="./crszsslw2412c09c1-8ea9-46bd-8063-084ccdd2f332.jpg" alt="LunarWing" width="400">
</p>

## LunarWing v2

The v2 line introduces the new execution engine, a browser-based multi-tenant onboarding console, richer gateway interactions, and a refreshed foundation for skills, memory, workers, and tools.

## Quick links

- [Website](https://lunarwing.org/)
- [Documentation index](docs/README.md)
- [Community guide](COMMUNITY.md)
- [Project manifesto](MANIFESTO.md)
- [Canonical source on GitLab](https://git.lunarwing.org/lunarwing/lunarwing2)
- [IRC: #lunarwing on Libera.Chat](https://web.libera.chat/?channel=#lunarwing)
- [AGPL-3.0-or-later license](LICENSE)

## Why LunarWing

- **Operator controlled** — run the agent, database, providers, workers, and communication bridges on infrastructure you control.
- **Open communication** — prioritize XMPP, IRC through WeeChat, DarkIRC, and other open or self-hostable paths.
- **Extensible by design** — add sandboxed WASM tools and channel adapters without folding every integration into the core daemon.
- **Secret-aware operations** — encrypted secret storage and tenant-scoped operational tooling are built into the deployment model.
- **Production multi-tenancy** — isolate tenants with separate OS users, services, databases, ports, configuration, and optional workers.
- **Reliable automation** — scheduled routines, retry and backoff, health checks, and self-healing tooling support unattended operation.
- **Lunarpunk values** — the core and bundled extensions remain free software under AGPL-3.0-or-later.

## Key v2 capabilities

### Engine V2 and the gateway

Engine V2 provides the channel-neutral execution path used by the v2 gateway, including streaming events, interrupts, stop handling, approval gates, and richer tool-result presentation. New multi-tenant configurations enable Engine V2 by default; direct single-instance configurations must opt in explicitly.

The gateway receives the full interactive experience. XMPP, DarkIRC, and WeeChat can opt in through `ENGINE_V2_CHANNELS`; other channels remain on the legacy path. Opted-in WASM channels currently receive terminal responses and status updates rather than live token edits. Validate channel-specific behavior before enabling Engine V2 for group channels.

Architecture details: [Engine V2](docs/architecture/ENGINE-V2.md).

### Channels and interfaces

- Browser gateway, CLI, and HTTP interfaces
- WASM channels for XMPP, WeeChat, DarkIRC, and Multica
- Optional Gotify notifications and SSH tooling

DarkIRC is opt-in. It is neither rendered nor built for a new tenant unless the operator enables it during tenant creation and builds its supporting service.

### External workers

LunarWing can attach persistent NanoCode, Pebble, or OpenCode worker containers. Worker choice is stored when the tenant is created. Matching `build-tenant --with-*` flags build the shared image; they do not change the worker selected for an existing tenant.

### Model providers

| Provider path | Intended use |
| --- | --- |
| `openai_compatible` | OpenAI-compatible endpoints, including compatible routing gateways |
| `ollama` | Local Ollama deployments |

## Get started with the onboarding GUI

The browser-based onboarding console is the recommended setup path for LunarWing v2. It wraps the multi-tenant administration scripts while showing configuration, command output, validation results, and recovery guidance in one local interface.

Run these commands from the repository root.

### 1. Explore safely in demo mode

```bash
./lunarwing_mt_onboard_web/run.sh --demo