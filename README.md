# LunarWing

## What is LunarWing?

LunarWing is a self-hosted, privacy-focused AI agent runtime written in Rust. It connects agents to open communication protocols, extensible tools, persistent memory, and infrastructure you control. Security boundaries, approvals, secret handling, and recoverable operation are part of the platform, not deployment afterthoughts.

<p align="center">
  <img src="docs/assets/branding/lunarwing-overview.jpg" alt="LunarWing" width="1400">
</p>

## Quick links

- [Website](https://lunarwing.org/) · [Blog](https://blog.lunarwing.org/)
- [Documentation index](docs/README.md)
- [Community guide](COMMUNITY.md) · [Project manifesto](MANIFESTO.md)
- [Canonical source on GitLab](https://git.lunarwing.org/lunarwing/lunarwing2)
- Companion application Repositories (PRE-ALPHA): [LunarWing Desktop](https://git.lunarwing.org/lunarwing/lunarwing2desktop) · [LunarWing Android](https://git.lunarwing.org/lunarwing/lunarwing2droid)
- [IRC: #lunarwing on Libera.Chat](https://web.libera.chat/?channel=#lunarwing)
- [AGPL-3.0-or-later license](LICENSE)

## Why LunarWing

- **Operator controlled** — the agent, database, providers, workers, and bridges run on infrastructure you control.
- **Open communication** — XMPP with OMEMO encryption, IRC through WeeChat, DarkIRC, and other open or self-hostable paths.
- **Extensible** — sandboxed WASM tools and channels, external MCP servers, and worker containers, without folding every integration into the core.
- **Secret-aware** — encrypted secret storage and tenant-scoped operational tooling are built in.
- **Multi-tenant ready** — tenants get their own OS users, services, databases, ports, and optional workers.
- **Reliable automation** — routines, retries, health checks, and self-healing for unattended operation.
- **Free software** — the core and bundled extensions are all AGPL-3.0.

## What's inside

**Engine and gateway.** Engine V2 is the channel-neutral execution path behind the browser gateway and eligible bridged channels, with streamed events, approval gates, and rich tool results. Multi-tenant setups route XMPP, WeeChat, and DarkIRC through it by default; single instances opt in. Details: [Engine V2](docs/architecture/ENGINE-V2.md).

**Live interaction.** `/interrupt` stops an in-flight LLM stream or tool batch, and `/steer <note>` redirects a running turn after its current tool call completes. Both work through the browser gateway, XMPP, WeeChat, and the REPL. See the [cross-channel live verification](docs/ops/v2.1.0.0_GOAL9_STEER_INTERRUPT_LIVE.md).

**Channels, tools, and workers.**

- Browser gateway, CLI, and HTTP interfaces
- WASM channels for XMPP, WeeChat, DarkIRC, and Multica, with XMPP file attachments via XEP-0363
- MCP servers over HTTP(S), Unix sockets, host stdio, and sandboxed WASM components
- Persistent Pebble, OpenCode, and Kimi Code worker containers, plus an extensible ACP for wiring up your own open source harnesses
- Optional extras: [LunarVoice](projects/voice-sidecar/README.md) speech and voice activity detection, [LunarVision](docs/guides/VISION_OCR_SIDECAR.md) OCR and vision-language analysis, Gotify notifications, GitLab and SSH tooling, and NetBird tunnels

**A real security model for tools.** MCP-over-WASM servers run as sandboxed component actors with no ambient filesystem, network, or process access, bounded by memory, fuel, and time limits. Host stdio servers can be sandboxed with seccomp, Landlock, and network isolation, and pinned artifacts are verified before execution. An optional marketplace trust stack adds signed manifests, dependency lockfiles, and revocation lists. Automatic npm/pip package installation stays disabled by design. Details: [MCP overall status](docs/ops/MCP_OVERALL_STATUS.md).

**Memory and learning.** Workspace memory gives agents durable, file-like documents with hybrid keyword and vector search. The native [Study-Notes knowledge store](ic/src/workspace/README.md#study-notes-knowledge-store) adds provenance-aware notes with staleness tracking.  Skills provide reusable prompt-driven capabilities, with optional self-improvement missions that stay proposals until a user approves them.

**Automation and health.** An optional reflex compiler turns recurring prompts into WASM fast paths. An opt-in [multi-agent coordination pipeline](docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md) adds policy, secret detection, and rate limiting for designated rooms. The bundled [infrastructure health system](ic-infrastructure-health-check/README.md) checks and can remediate services with backoff and flapping protection, and exports Prometheus metrics — on both systemd and OpenRC.

**Model providers.** Local Ollama and OpenAI-compatible endpoints (including gateways like TensorMoon). Operators can add or override providers declaratively, without touching Rust.

## Getting started

The browser-based onboarding console is the recommended setup path for beginners. It wraps the multi-tenant admin scripts and shows configuration, output, validation, and recovery guidance in one local interface. Run from the repository root:

```bash
# 1. Look around safely — no tenants, no host changes
./lunarwing_mt_onboard_web/run.sh --demo

# 2. Check the host first
sudo ic/scripts/lunarwing-mt-admin.sh doctor

# 3. Start the real console
sudo ./lunarwing_mt_onboard_web/run.sh
```

Open the token-bearing URL it prints (normally `http://127.0.0.1:1969/?token=...`) and keep the process running until the workflow finishes.

You will need Python 3.10+, a GNU/Linux host running systemd or OpenRC, root or sudo, Docker or Podman, `jq`, `git`, and network access on first launch. See the [distribution-specific prerequisites](docs/guides/) and the [full GUI reference](lunarwing_mt_onboard_web/README.md).

Worth knowing before you provision:

- The gateway binds to `127.0.0.1` by default.
- New tenants enable Engine V2, SSH support, health checks, build, and start. DarkIRC, Gotify, external workers, and optional toolchains stay off until selected.
- Blank XMPP fields assign `<tenant>@xmpp.localhost` and provision the bridge automatically.
- The onboarding server is a privileged local console for one trusted operator: no user accounts, no TLS. Keep it on loopback with token auth (real mode refuses non-loopback binding), use an SSH tunnel for remote admin, and treat session URLs and audit files as credentials. Docker group membership is effectively root-equivalent.

Prefer the terminal? The [Python onboarding CLI](lunarwing_mt_onboard/README.md) and [direct mt-admin](docs/guides/MT-ADMIN-QUICKSTART.md) cover the same ground with exact flag-level control.

## Upgrades and migration

The GUI upgrade workflow wraps `upgrade-tenant`, accepting a branch, tag, or commit, with PostgreSQL backup and unit rendering on by default.

Moving a v1 tenant to v2 uses a staged Kawarimi export/import: rehearse on a separate v2 deployment, validate the restored tenant, and stop the old daemon and bridge before cutover when both sides share an XMPP identity. Bundles are AES-256 encrypted, and import mints fresh gateway and webhook tokens. Back up first and delete bundles after verification. Runbooks: [machine migration](docs/ops/MT-MACHINE-MIGRATION.md), [legacy notes](docs/ops/MT-LEGACY-UPGRADE-NOTES.md).

## Development and testing

The Rust workspace lives under `ic/` (Rust 1.96.1, edition 2024). Its crates.io dependency closure is committed under `ic/vendor/` and source-replaced through Cargo; review the [vendoring rules](AGENTS.md#vendored-dependencies) before changing dependencies. On a resource-constrained host, cap Cargo at twelve CPUs/jobs and prefer `cargo check` over full debug builds:

```bash
cd ic
taskset -c 0-11 cargo fmt --all -- --check
taskset -c 0-11 cargo check -j12
taskset -c 0-11 cargo test -j12 -- --test-threads=12
```

More surfaces: [Rust and browser E2E](ic/tests/e2e/README.md) · [external worker harness](tests/README.md) · [testing guide](docs/guides/TESTING_GUIDE.md) · [XMPP harness](ic/testing/lunarwing-xmpp/README.md).

## Documentation

| Area | Reference |
| --- | --- |
| Architecture | [docs/architecture/](docs/architecture/) |
| Operator guides | [docs/guides/](docs/guides/) |
| Production operations | [docs/ops/](docs/ops/) |
| Release notes | [docs/releases/](docs/releases/) |
| Known bugs and status | [docs/bugs/README.md](docs/bugs/README.md) |

## Project and community

LunarWing focuses on user freedom, self-hostable infrastructure, and open protocols. Read the [manifesto](MANIFESTO.md) for the philosophy and the [community guide](COMMUNITY.md) for how to participate.

[![Chat on IRC](https://img.shields.io/badge/IRC-%23lunarwing-00b0aa?style=for-the-badge&labelColor=000000)](https://web.libera.chat/?channel=#lunarwing)

LunarWing is entirely self-funded and maintained by a thriving community of volunteers, ranging from individuals to small organizations. Everything is licensed under [AGPL-3.0-or-later](LICENSE).
