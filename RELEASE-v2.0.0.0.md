# Release Notes for LunarWing v2.0.0.0 — Codename `Kosoku`

**Release Date:** TBD

> Codename *Kosoku* evokes speed. LunarWing v2 makes interaction feel faster through native response streaming, responsive interrupts, durable tool activity, and a substantially expanded operator experience. The larger change is architectural: Engine V2 moves from groundwork present in the final v1 line to an active, daemon-wired execution path built around threads, capabilities, policies, gates, projects, and durable memory.

## Overview

LunarWing v2.0.0.0 is the first major release after v1.1.9.0. It combines the Engine V2 runtime integration with native LLM streaming, channel-neutral delivery, improved approval and cancellation behavior, a browser-based multi-tenant onboarding application, host-local stdio MCP support, a new skill-maintenance foundation, and a broad set of worker, migration, UI, dependency, and repository changes.

This planned release uses two related version forms:

- The planned release, tag, and release notes use `2.0.0.0`.
- Rust crates and package manifests use `2.0.0`.

The v2 repository restarts Git history, so these notes were prepared from an exact tree comparison against the LunarWing v1 `v1.1.9.0` tag, followed by a review of the native v2 history and current source. The change inventory below describes material runtime, operator, compatibility, security, and contributor changes rather than treating the disconnected repositories as a normal Git commit range.

## Highlights

- Engine V2 is now wired into the daemon execution path, with event-sourced threads, capability leases, policy evaluation, execution gates, project-scoped memory, and CodeAct support.
- Native typed LLM streams carry text and fragmented tool calls through the provider and Engine layers, with bounded retry/failover behavior and prompt cancellation.
- `/interrupt` and `/stop` remain responsive during active Engine turns; normal messages are deferred through a bounded FIFO queue.
- Gateway users receive live response deltas, durable tool-call panels, approval/authentication prompts, and restored tool state after reload.
- New multi-tenant environments enable Engine V2 for the gateway by default; existing and non-managed deployments remain explicitly configurable.
- The new localhost onboarding web application covers provision, secrets, upgrade, export, and import flows with live logs and redacted audit records.
- Multi-tenant worker selection is persisted per tenant, and tenant start-up no longer depends on whichever shared worker images happen to exist.
- Host-local stdio MCP servers can be installed and managed alongside HTTP and Unix-socket MCP transports.
- The dedicated OpenAI Codex/ChatGPT-subscription OAuth backend has been removed; Codex model IDs remain usable through a compatible API endpoint.
- Automated skill self-improvement and maintenance foundations are present but deliberately disabled by default; explicit proposal resolution and registry publication use separate safeguards.
- Pebble source is included in the v2 repository, while the current Pebble worker image build still clones its upstream source.
- The v2 source repository is hosted on Codeberg; the prior GitHub Actions suite is not part of this tree.

## Compatibility at a Glance

| Area | v2.0.0.0 behavior |
|---|---|
| Release version | `2.0.0.0` |
| Crate/package version | `2.0.0` |
| Tree-comparison baseline | LunarWing v1 tag `v1.1.9.0`; this does not establish a supported in-place upgrade path |
| Engine V2 in the binary | Off unless `ENGINE_V2=true` |
| Newly rendered multi-tenant environments | `ENGINE_V2=true` |
| Gateway routing | Uses Engine V2 whenever Engine V2 is enabled |
| XMPP, DarkIRC, and WeeChat routing | Legacy by default; opt in by exact name through `ENGINE_V2_CHANNELS` |
| Other channels | Continue through the legacy path |
| Automated skill self-improvement | Off unless `SKILL_SELF_IMPROVEMENT=true` |
| Dedicated OpenAI Codex OAuth backend | Removed |
| OpenAI-compatible endpoints | Supported through `openai_compatible` |
| New SQL migration files since v1.1.9.0 | None found in the compared tree |

## Changes

### Engine V2 Runtime Integration

The final v1 line already contained the initial Engine V2 crate and its thread, capability, policy, project, gate, CodeAct, mission, and circuit-breaker groundwork. v2.0.0.0 activates a parallel daemon execution path that actively uses that model.

- Daemon conversations can now run as event-sourced Engine threads with validated lifecycle transitions.
- The daemon-wired path applies scoped, expiring, use-limited capability leases and deterministic policy decisions.
- Structured tool calls and Python CodeAct execution use the Engine's common lease, policy, gate, and result pipeline.
- Projects scope live daemon threads, missions, and durable `MemoryDoc` knowledge.
- Thread events now carry response deltas, action lifecycle events, gate state, failures, and terminal state changes.
- Gateway connections can adopt the client-subscribed thread ID, improving consistency between live delivery and restored history.
- Workspace identity and persona are injected into Engine prompts at Engine initialization.
- Engine terminal responses are delivered once through the normal outbound hook path; pauses and stops avoid duplicate terminal messages.

Engine V2 remains a parallel, environment-gated path. It is not yet a universal replacement for every legacy channel or dispatcher behavior. See [Engine V2 Architecture](docs/architecture/ENGINE-V2.md) for the internal model.

### Native LLM Streaming and Reliability

- The provider contract now supports typed native stream chunks for text, tool calls, and completion, while stream errors remain typed `Result` failures.
- The Rig adapter was updated for `rig-core 0.40` and maps native provider streams into LunarWing's provider-neutral stream format.
- Engine stream collection reconstructs plain text, CodeAct code, and fragmented structured tool calls.
- Live response deltas are broadcast while a turn runs; only a successfully completed response is committed as the assistant result.
- Malformed or incomplete tool-call streams fail closed instead of executing partial calls.
- Prompt acquisition and active streams can be cancelled without committing usage or an incomplete assistant response.
- Retry and failover decorators may retry before the first visible chunk. Once output is visible, later failures are surfaced rather than replayed and duplicating text.
- Response caching avoids storing incomplete streams; tool-call streams bypass the text response cache.
- Timeout, recording, circuit-breaker, smart-routing, retry, and failover decorators preserve the streaming contract.
- Non-streaming providers remain compatible through a fallback that emits the completed response as a final delta.
- Newly rendered tenant environments default the LLM circuit breaker to seven fully retried failures and a 45-second recovery window. Existing explicit values are preserved.

The detailed design and delivery work is documented in [the Engine streaming plan](docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-5.md).

### Responsive Interrupts, Queuing, and Timeouts

- Exact `/interrupt` and `/stop` submissions bypass an active turn and are dispatched with priority.
- An interrupt cancels the active provider future or stream for the targeted conversation.
- Ordinary messages received during an active turn enter a FIFO deferred queue with a capacity of 256.
- Queue overflow produces an explicit busy response rather than silently discarding the submission.
- Deferred messages retain their order and conversation scope.
- Other commands, including `/clear`, remain normal queued submissions rather than priority interrupts.
- A soft turn timeout detaches and suppresses the handler; a hard-kill grace period follows before forced cancellation unless the user interrupts explicitly.

### Channel-Neutral Delivery and Execution Gates

- Engine conversation keys include the channel and conversation scope so that similarly named scopes on different channels do not collide.
- The gateway is automatically eligible when Engine V2 is enabled.
- XMPP, DarkIRC, and WeeChat opt in only when their exact names appear as trimmed, case-insensitive comma-separated `ENGINE_V2_CHANNELS` entries.
- Unknown names and empty entries are ignored; CLI, HTTP, and other channels continue on the legacy path.
- Approval and authentication requests are emitted through the same channel-status interface used for progress and results.
- Existing pending-gate persistence is used so internal Engine resolution can survive daemon restart.
- CodeAct calls now preserve structured gate context, successful sibling results, call identity, and action identity across a pause.
- Approved actions use a one-shot resume path so the action executes exactly once.
- Ambiguous conversation-scope gate matches fail closed.
- Resolved action events are retained for history reconstruction and tool panels.
- The WASM channel wrapper currently drops incremental `StreamChunk` updates; those channels receive terminal output and non-token progress/gate status events.

See [Channel-Neutral Engine V2 Delivery](docs/superpowers/specs/2026-07-12-engine-v2-channel-neutral-delivery-design.md).

### Gateway UI and Tool Activity

- Streaming text is rendered as plain text while it changes and converted to final Markdown once at completion.
- Animation-frame batching reduces repeated DOM work during rapid token and log updates.
- Thread rows are keyed and updated in place instead of being rebuilt on every event.
- History requests are sequenced to prevent older responses from overwriting a newly selected thread.
- Scroll, highlighting, and message rendering are scheduled to avoid UI stalls and stale controls.
- Live tool calls show bounded result previews and resolved status.
- Completed and resolved tool-call summaries are reconstructed from persisted Engine events after a reload.
- Retained history bounds older tool previews and errors for compatibility with existing stored turns.
- Gateway logs are batched and capped in the client.
- Google-hosted font dependencies were removed; the gateway and onboarding UI use self-hosted fonts.
- Updated loading, status, mascot, and control animations accompany the v2 visual refresh.

### Skills, Memory, and Self-Improvement Foundations

- V2 skills use durable Engine `MemoryDoc` records with activation metadata, provenance, confidence metrics, versions, hashes, and optional CodeAct snippets.
- Existing skill-extraction and conversation-insight missions feed successful work into reusable project knowledge.
- The new B-1 flow can prepare skill patch proposals from measured failures.
- B-2 adds confidence-based demotion and prune proposals for generated skills.
- Authored and installed skills are protected from automatic demotion and pruning.
- Patch, prune-to-archive, and update proposal records use explicit review and approval; registry publication is a separate explicit action.
- Gateway endpoints and proposal cards expose the new maintenance workflow.
- Registry publication on the backend requires trusted/proven metadata, a `CLAWHUB_TOKEN`, eligibility checks, and leak scanning of skill bodies and code snippets.
- Registry-fetched skills are leak-scanned before installation.

Automated failure collection, proposal staging, maintenance-mission registration, inline demotion, and orchestrator proposal hooks are disabled by default behind `SKILL_SELF_IMPROVEMENT=false`. Explicit proposal-resolution and publication APIs are not controlled by that flag; they use ownership, eligibility, leak-scan, and token checks. The separate Python-orchestrator self-modification path also remains disabled by default. Skill extraction and conversation insights are distinct from those mutation gates.

The B-3 update and browser-publish paths are not complete end-to-end in the current draft tree; they are listed under Known Issues rather than presented as finished automatic updating. See [Self-Improving Skills B-2/B-3](docs/plans/SELF_IMPROVING_SKILLS_B2_B3.md).

### LLM Providers and Model Routing

- Removed the dedicated `openai_codex` implementation that authenticated with a ChatGPT subscription/device-code session.
- Removed Codex token refresh, persisted session, login, and backend-specific environment handling.
- `openai_codex`, `openai-codex`, and `codex` backend aliases now fail with migration guidance.
- `lunarwing login --openai-codex` is rejected; bare `lunarwing login` directs users to provider onboarding.
- Operators migrating from the removed backend should use `LLM_BACKEND=openai_compatible` with an appropriate `LLM_BASE_URL`, `LLM_API_KEY`, and `LLM_MODEL`.
- ChatGPT subscription/device OAuth and Codex CLI `auth.json` credentials cannot be reused through `openai_compatible`. Remove `LLM_USE_CODEX_AUTH`, `CODEX_AUTH_PATH`, and `OPENAI_CODEX_*` settings and provision an API endpoint/key with its own authentication and billing.
- This does not prohibit Codex-named models offered by an OpenAI-compatible endpoint.
- Direct `openai` / `open_ai` backend names remain rejected, as they already were in v1.1.9.0.
- LunarWing Cloud remains a separate special backend.

### Host-Local stdio MCP

Host-local stdio MCP configuration and managed-process support, absent from the v1.1.9.0 baseline and present in v2, now spans the typed registry, CLI, conversational installation tools, web settings, config persistence, startup loading, and shutdown handling.

- Install definitions support an executable command, structured argument list, and non-secret environment entries.
- Validation rejects empty commands, NUL bytes, and malformed environment names or values.
- The configured executable and arguments are invoked directly without an implicit shell.
- Installation persists configuration without requiring the child server to be reachable immediately.
- CLI install, remove, and toggle operations update persisted configuration; a running daemon must be restarted to pick up those CLI-only changes.
- Web/conversation lifecycle handling can activate a definition live. Removing one through that live extension manager unregisters its tools, stops the managed child process, and deletes its configuration.
- HTTP and Unix-socket MCP transports remain available.
- OAuth remains limited to HTTP MCP servers.

stdio MCP processes run on the LunarWing daemon host, outside worker containers and outside the WASM sandbox. Environment values are stored as ordinary configuration, not encrypted secrets. Package installation, worker-local stdio MCP, bundle-carried MCP entries, and a complete CLI activate/deactivate lifecycle are not included. See [Host-Local stdio MCP Installation](docs/superpowers/specs/2026-07-09-host-local-stdio-mcp-installation-design.md).

### Browser-Based Multi-Tenant Onboarding

Added `lunarwing_mt_onboard_web/`, a FastAPI/Uvicorn browser interface over the existing Python onboarding modules and `lunarwing-mt-admin.sh` source of truth.

- Five guided flows cover provision, secrets, upgrade, export, and import.
- The upgrade screen wraps the legacy v1/rootful-Docker workflow; it is not a supported v1-to-v2 upgrade path. Its target/preflight defaults are also v1-oriented when no explicit target is supplied.
- WebSocket events stream logs, phase progress, results, and cancellation state.
- Cancellation terminates the complete child process group rather than only the immediate wrapper process.
- A random session token protects REST and WebSocket calls by default.
- Per-job audit logs redact known secret flags, values, and master-key forms.
- Upgrade, export, and import retain dry-run-first behavior.
- Import remains staged by default; starting a restored tenant is a separate action and unattended start requires old-host-stopped confirmation.
- Demo mode replaces privileged scripts with safe simulations for all five workflows.
- The browser UI adds progress views, tips, a moon indicator, and an animated bat mascot without adding a frontend build toolchain.

Quick start:

```bash
# Safe demonstration mode
./lunarwing_mt_onboard_web/run.sh --demo

# Real provisioning and tenant operations
sudo ./lunarwing_mt_onboard_web/run.sh
```

The service is intended for loopback use. Do not expose this privileged UI to an untrusted network. Protect its audit-log directory with restrictive ownership and modes; files inherit the process umask, and pattern-based redaction is not exhaustive. See [LunarWing MT Onboard — Web Edition](lunarwing_mt_onboard_web/README.md).

### Multi-Tenant CLI, Secrets, and Migration

- Added `sudo python3 -m lunarwing_mt_onboard secrets` for inserting encrypted secrets into a tenant PostgreSQL store.
- Interactive secret values are entered through hidden prompts.
- The Python implementation uses AES-256-GCM with HKDF-SHA256 derivation compatible with the Rust runtime.
- Tenant environment discovery supplies the database URL, master key, and owner ID without manual exports.
- Conflict-safe writes update an existing secret of the same name.
- The web import flow reuses the Python `import_tenant.py` argument builder and the existing Kawarimi `import-tenant.sh` implementation.
- Rootless Podman export now detects the tenant-owned container and runs inspection and `pg_dump` with the tenant's `HOME` and `XDG_RUNTIME_DIR`.
- Post-start verification delegates to `lunarwing-mt-admin.sh status`, supporting systemd user units and OpenRC rather than assuming `systemctl`.
- Kawarimi remains a staged, PostgreSQL-based cutover with tenant downtime and explicit owner-scope reconciliation.
- No new SQL migration files were introduced between the v1.1.9.0 baseline and this v2 tree.

The Python CLI does not currently expose `import` as a top-level subcommand; use the web flow or `ic/scripts/import-tenant.sh` directly. See [the multi-tenant CLI guide](lunarwing_mt_onboard/README.md).

### Per-Tenant Workers and Health Monitoring

- `add-tenant` accepts `--with-nanocode`, `--with-pebble`, and `--with-opencode` and persists the selected worker map in `/etc/lunarwing/ports.json`.
- `start-tenant` starts only the workers selected for that tenant.
- Onboarding, Kawarimi import, tenant registration, and build flows carry the same worker choices.
- Existing tenants without a worker map default to all workers off. Re-running `add-tenant TENANT --with-...` can enable required workers, but it also re-renders tenant environment/service configuration; back up the tenant and repeat all existing feature flags first.
- Worker selection is currently enable-only and idempotent; there is no matching disable verb.
- LunarVision health monitoring discovers every tenant's `vision_health` port from the registry and reports the worst aggregate state.
- `HEALTH_LUNARVISION_URL` remains available as an explicit single-target override.
- When registry discovery is unavailable, the health checker retains its localhost fallback.

### Worker and Tool Fixes

- Structured OpenCode and NanoCode `project_dir` values now expand `~` and `~/...` relative to the worker's `/workspace` root.
- Absolute structured paths remain unchanged.
- `ssh_git` treats JSON null, blank strings, whitespace, and the textual value `"null"` as an omitted ref.
- Omitting a ref now allows normal remote-default-branch behavior without forwarding a bogus ref.
- External worker starts are controlled by the tenant's persisted selection rather than shared image availability.
- Job creation continues to support asynchronous execution with `wait=false`; synchronous waiting remains the default.

Path normalization is not universal: Pebble and prompt-generated paths retain their prior behavior. Operators should continue to pass an explicit `ssh_git` ref for bare repositories whose remote `HEAD` is invalid or points to a missing branch.

### Channels and Bridges

- XMPP, WeeChat, DarkIRC, MultiCA, and associated bridge/channel packages now report version `2.0.0`.
- XMPP and WeeChat metadata round-trip coverage was expanded.
- The standalone XMPP bridge lockfile was repaired so local LunarWing packages resolve consistently at `2.0.0`.
- Gotify has no material runtime delta from the v1.1.9.0 baseline.
- DarkIRC secure key exchange remains a proposal and is not part of v2.0.0.0. Existing operators still manage contact keys manually.
- The daemon continues to offer `lunarwing-agent-v1` and the temporary `ironclaw-agent-v1` compatibility protocol.
- The old root worker/channel symlinks retained in v1.1.9.0 are still present in the current v2 tree.

See [the DarkIRC key-exchange proposal](docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md) for future work; it is not shipped functionality.

### Pebble Source and Worker Packaging

- The v1 Pebble gitlink is replaced in the v2 tree by a full vendored Pebble Rust workspace.
- This makes the referenced Pebble source directly inspectable in the LunarWing v2 checkout.
- The existing `pebble4lunarwing` image build does not yet consume that vendored tree; it still clones the upstream repository at build time.
- Pebble worker image builds therefore remain network-dependent and are not pinned by the newly vendored source alone.

### Security and Runtime Hardening

- Recorded HTTP replay is checked before DNS resolution while request parsing, private-address validation, and leak scanning remain enforced.
- Hermetic replay tests can therefore use recorded unresolvable hosts without weakening live request validation.
- Gate resolution preserves action identity and executes an approved action only once.
- Ambiguous gate ownership and scope resolution fail closed.
- Skill publication and registry installation paths add content and code-snippet leak scanning.
- Web onboarding defaults to loopback and token protection and redacts known secret forms from audit logs.
- Stdio MCP uses an exact executable/argument configuration and does not invoke an implicit shell. An operator can still explicitly configure a shell such as `sh -c`, so every stdio definition remains trusted host-code execution.
- Windows Wasmtime cache configuration was updated to the current format.

### Dependencies, Tests, Documentation, and Repository Operations

- Main LunarWing crates, internal crates, XMPP bridge/channel, MultiCA, DarkIRC, and WeeChat relay manifests move to `2.0.0`.
- `rig-core` moves from `0.30` to `0.40` for native stream support.
- `serde_yml` is replaced by `serde_norway 0.9`, and the obsolete advisory exception is removed.
- The direct `eventsource-stream` dependency is removed.
- The XMPP bridge lockfile is aligned with the v2 package versions.
- Targeted test coverage was added for native streams, decorators, cancellation, channel routing, interrupts, gates, tool panels, skill proposals, tenant worker selection, onboarding, migration flags, and multi-tenant health discovery.
- Recorded-HTTP replay, WASM cache configuration, MCP URL expectations, and OpenClaw import provider tests were repaired during v2 development.
- Documentation was reorganized into architecture, plans, specifications, release archives, operational guides, and reconciled bug-status areas.
- The v1.1.9.0 release note moved unchanged into [the release archive](docs/releases/RELEASE-v1.1.9.0.md).
- Contributor orchestration helpers and Codex-focused development guidance were added under `.claude/`; older root and per-directory `CLAUDE.md` files were removed.
- The prior `.github` workflows, Dependabot configuration, funding metadata, and GitHub issue/PR templates are absent from the v2 tree.
- A Forgejo workflow mirrors the Codeberg repository to GitHub.

The current Forgejo workflow is a mirror, not a replacement build, test, coverage, or release pipeline. Follow [the Codeberg release-command draft](docs/ops/RELEASE-COMMANDS-CODEBERG.md) cautiously; it still identifies unverified release steps.

## Bug Fixes and Polish

- Fixed Engine stop handling during both stream acquisition and active streaming.
- Fixed gate context loss for CodeAct calls and preserved successful sibling action results across an approval pause.
- Fixed approved actions potentially passing through the normal gate path a second time on resume.
- Fixed duplicate or missing terminal delivery across gateway, XMPP, DarkIRC, and WeeChat routing paths.
- Fixed stale gateway history responses overwriting the currently selected thread.
- Fixed high-frequency stream/log rendering that caused unnecessary DOM churn and UI lag.
- Fixed rootless-Podman export commands running in the wrong user's runtime context.
- Fixed tenant verification assuming systemd instead of using the multi-init status abstraction.
- Fixed all available shared worker images starting for every tenant regardless of tenant selection.
- Fixed structured `~` path handling for OpenCode and NanoCode.
- Fixed null-like `ssh_git` refs being forwarded as literal branch names.
- Fixed recorded HTTP fixtures failing before replay because of live DNS resolution.
- Fixed current Wasmtime cache configuration on Windows.
- Fixed standalone XMPP bridge package versions drifting from the main v2 workspace.

## Upgrade Notes

1. **Back up PostgreSQL and tenant files before any v2 change.** No new SQL migration file was found in this comparison, but v2 changes runtime behavior, provider configuration, worker selection, and configuration serialization paths.
2. **Treat `2.0.0.0` as the release/tag version and `2.0.0` as the crate version.** Scripts that validate only three-part semantic versions may not accept the four-part release tag.
3. **Do not use the current onboarding upgrade wizard with `v2.0.0.0`.** Its target validator currently rejects four-part versions, and a supported v1-to-v2 in-place path has not been established in this release tree. Provision v2 separately and use a reviewed, staged Kawarimi migration until an in-place path is release-tested.
4. **Migrate the removed Codex OAuth backend before starting the daemon.** Replace `openai_codex` / `codex` with `openai_compatible`, remove the old Codex-auth environment, and configure a separately authenticated/billed endpoint, API key, and model. Subscription/device OAuth and Codex CLI credentials are not reusable.
5. **Choose Engine V2 activation intentionally.** Newly rendered tenant environments set `ENGINE_V2=true`; existing environments are not silently rewritten. Standalone deployments must set it explicitly.
6. **Keep non-gateway channels on the legacy path until their caveats are acceptable.** Add only `xmpp`, `darkirc`, and/or `weechat` as trimmed, case-insensitive `ENGINE_V2_CHANNELS` entries; do not opt group channels in until the memory-context limitation below is resolved or mitigated.
7. **Re-select external workers for existing tenants carefully.** A missing worker map means NanoCode, Pebble, and OpenCode all remain off. There is no dedicated worker-enable verb: re-running `add-tenant` rewrites environment and service configuration. Back up the tenant and repeat every existing feature flag before adding the required `--with-...` values.
8. **Review MCP trust boundaries.** Stdio MCP commands run on the daemon host and their configured environment is stored in ordinary config. Do not place secrets in stdio MCP environment entries.
9. **Back up worker and DarkIRC configuration before settings rewrites or environment regeneration.** Current serialization and renderer limitations can discard worker bearer tokens or manually managed DarkIRC contacts.
10. **Expect Pebble builds to use the network.** The included source is not yet wired into `pebble4lunarwing` image creation.
11. **Use an explicit Git ref for fragile bare repositories.** Null-ref handling is fixed, but an invalid remote `HEAD` can still produce an empty or unexpected checkout.
12. **Run the release checks manually.** The prior GitHub CI/release workflows are gone and the current Forgejo workflow mirrors source only.

### Configuration Quick Reference

| Setting | Default / generated value | Effect |
|---|---|---|
| `ENGINE_V2` | Binary default `false`; newly rendered tenants `true` | Enables the Engine V2 daemon path |
| `ENGINE_V2_CHANNELS` | Empty | Keeps XMPP, DarkIRC, and WeeChat on the legacy path; gateway remains Engine-eligible |
| `SKILL_SELF_IMPROVEMENT` | `false` | Gates automated collection, proposal staging, maintenance missions, inline demotion, and orchestrator proposal hooks |
| `ORCHESTRATOR_SELF_MODIFY` | Disabled | Keeps Python orchestrator self-modification inactive |
| `LLM_CIRCUIT_BREAKER_THRESHOLD` | Newly rendered tenants: `7` | Opens the breaker after repeated fully retried failures |
| `LLM_CIRCUIT_BREAKER_RECOVERY_SECS` | Newly rendered tenants: `45` | Controls the open-to-probe recovery interval |
| `CLAWHUB_TOKEN` | Unset | Required by the backend registry-publish path |
| New-tenant external workers | None selected | Requires explicit `--with-nanocode`, `--with-pebble`, and/or `--with-opencode` |
| New-tenant Docker group access | Enabled by the Python provisioner | May grant root-equivalent host access; disable unless the tenant requires Docker access |
| New-tenant TensorZero URL | `http://192.168.1.157:3000/openai/v1` | Site-specific private default; replace it outside that deployment |
| New-tenant model | `tensorzero::function_name::lunarwing` | Requires a matching TensorZero/OpenAI-compatible endpoint configuration |

## Release Verification Status

Fresh draft-time checks run on July 15, 2026:

- `cargo test --locked -p lunarwing_engine -p lunarwing_skills`: 505 tests passed (`344` Engine and `161` skills), with no failures.
- Engine channel-delivery and interrupt-ingress integration tests with `libsql,integration`: 6 tests passed, with no failures.
- Combined onboarding, upgrade, secrets, import, verification, and web unit suite: 130 tests passed, with no failures.
- Per-tenant worker-selection shell regression suite: passed.
- Kawarimi import/export flag regression suite: passed.
- LunarVision health suite: 81 checks passed, with no failures.
- Gateway `app.js` syntax check: passed.
- Shell syntax checks for `lunarwing-mt-admin.sh`, import/export, and the web launcher: passed.

The Python suite emits an existing `ResourceWarning` for an unclosed file in `lunarwing_mt_onboard/provisioner.py`. The aggregate `test-mt-onboard.sh` wrapper also exits during its parser smoke check because it places a global `--non-interactive` option after the `upgrade` subcommand; the underlying 130-test suite passes when invoked correctly.

Full release sign-off is still pending. Before tagging v2.0.0.0, release engineering must run and record the locked full-workspace build/test/lint gates; live gateway and channel smoke tests; stdio MCP, migration, and external-worker validation; and package, container, multi-architecture, signature, checksum, and Codeberg release/upload verification. Do not infer a full green release gate from historical counts in planning documents.

## Known Issues

This is a code-verified release-note view of the current v2 tree. Design documents and proposal status were not treated as proof of implementation.

### High-Priority Pre-Release Caveats

- **Engine group-channel memory isolation is not at legacy parity.** Engine initialization currently builds a non-group workspace prompt once, including personal `MEMORY.md` content. The legacy dispatcher rebuilds context for group chats and excludes that personal memory. Do not opt XMPP, DarkIRC, or WeeChat group conversations into Engine V2 until this is fixed or the workspace contains no private memory.
- **Live JSON tool previews can bypass output sanitization.** Previews are bounded, but valid JSON tool output is currently reparsed from the raw result while plain text uses the sanitized fallback. Avoid exposing sensitive tool results through preview-enabled clients until the paths are unified.
- **Live tool parameter summaries are not sensitive-parameter-aware.** Bounded display labels can be derived directly from URLs, shell commands, message content, or an arbitrary string argument. Secrets embedded in those values may be shown to preview-enabled clients.
- **Gateway approval/authentication cards are not restored after browser reload.** Engine pending gates persist for internal resolution across daemon restart, but current gateway history loading does not reconstruct the waiting card.
- **The four-part v2 release target is rejected by the onboarding upgrade wrapper.** A supported v1-to-v2 in-place path is not established; use a staged fresh-install migration until the complete path is release-tested.
- **Browser skill publishing is not functional end to end.** The backend requires an Engine document UUID, but the current skill list response/UI does not reliably carry that identifier.
- **Registry skill update is scaffolding, not a content updater.** Update detection/proposal helpers have no production caller, and approving an update proposal changes metadata without fetching and replacing skill content.
- **There is no active build/test/release CI gate in this tree.** The only current Forgejo workflow mirrors the repository to GitHub.
- **Pebble packaging metadata is inconsistent.** The v2 tree vendors Pebble as a normal directory while `.gitmodules` still declares it as a submodule, and the worker image build clones upstream instead of using the vendored source.

### Open or Carried Forward

- Engine streaming does not retry or fail over after the first visible chunk, and partial streams are not durable or resumable after a process failure.
- XMPP, DarkIRC, and WeeChat do not display live incremental token edits through the current WASM channel interface; they receive terminal output and status events.
- The ordinary soft timeout suppresses the current handler before provider cancellation; forced cancellation follows after the hard-kill grace period unless the user interrupts.
- Pending Engine gates currently use a hard-coded 30-minute expiry rather than the configured five-minute supervised timeout.
- Commands other than exact `/interrupt` and `/stop`, including `/clear`, wait behind the active turn.
- Automated self-improvement collection and maintenance paths are off by default and have not been live-validated against a production skill registry. Explicit resolution/publication endpoints remain separately reachable when their checks pass.
- Skill proposal and publication access includes caller-owned and shared-owner skills; it is not a separate administrator-only boundary.
- The Python onboarding CLI has no top-level `import` subcommand. Use the web UI or `import-tenant.sh`.
- The onboarding web server can be bound off-loopback after a warning and can disable its token with `--no-token`; neither mode is appropriate on an untrusted network. Its token is carried in the launch URL and browser session storage.
- Web audit redaction is pattern-based, and audit files inherit the process umask rather than enforcing `0600`. Protect the log directory and avoid passing unrecognized secret forms on command lines.
- Noninteractive `python3 -m lunarwing_mt_onboard secrets --secretvalue ...` exposes the value in the process list. The parsed `--yes` flag is unused, noninteractive names bypass the interactive validation helper, and the implementation is PostgreSQL-specific with a standard `/home/<tenant>` assumption.
- Kawarimi import always builds and installs WASM; there is no user-facing `--with-wasm` option.
- Worker selection is enable-only; there is no matching command to turn an individual persisted worker flag off.
- Rootless export support specifically covers Podman, not rootless Docker.
- `jq` is a LunarVision health-check runtime prerequisite. With `jq` available but no usable registry target, discovery falls back to `127.0.0.1:8088`.
- External and sandbox job creation still defaults `wait` to `true`. Use `wait=false` for long work that should not block the current conversation turn.
- A later full TOML settings rewrite can omit external-worker bearer tokens because those fields are not serialized. Back up and verify worker blocks after settings changes.
- The vendored Pebble source is not used by the Pebble worker Dockerfile, which still clones upstream and remains network-dependent.
- Structured `~` expansion is implemented for OpenCode and NanoCode, not Pebble or arbitrary paths embedded in prompts.
- `ssh_git` still inherits normal Git behavior when a bare remote's `HEAD` points to a missing branch. Pass `ref` explicitly.
- XMPP turn processing can still apply backpressure to its inbound queue, and live OMEMO warm-up/MUC fallback behavior still needs release-environment verification.
- WeeChat random challenge checking is not implemented, and its current ISO-8601 timestamp parser returns a placeholder value.
- DarkIRC secure key exchange is not implemented. Manual contact keys remain necessary, and environment/config regeneration can overwrite manually managed contact sections.
- DarkIRC private messages retain conservative chunking/rate behavior because of DarkFi event metering limitations.
- The legacy `ironclaw-agent-v1` protocol alias and old root compatibility symlinks remain despite their planned v2 removal.
- Stdio MCP remains daemon-host-local, lacks worker execution and package installation, stores environment values unencrypted, and has no complete CLI activation/deactivation workflow.
- Registry bundles that contain MCP entries remain unsupported.
- The current release-command document contains unverified Codeberg steps and should not be treated as an automated release guarantee.
- `test-mt-onboard.sh` currently exits in its upgrade parser smoke check because `--non-interactive` is placed after a subcommand that does not define it there. Put global options before `upgrade` when invoking the CLI directly.
- Onboarding subprocess tests emit a `ResourceWarning` for an unclosed output stream in `provisioner.py`.

### Resolved Since v1.1.9.0

- The dedicated OpenAI Codex/ChatGPT-subscription backend, auth session, and token refresh path are removed with explicit migration errors.
- Null-like `ssh_git` refs no longer become literal branch names.
- Structured OpenCode and NanoCode project paths expand `~` against the workspace root.
- Rootless-Podman exports use the tenant user's container runtime and environment.
- Tenant post-start verification is no longer tied directly to systemd.
- Worker start-up honors per-tenant selection instead of shared image availability.
- Engine interrupts cancel stream acquisition and active streams without committing incomplete responses.
- CodeAct approval state preserves context, and approved actions execute once on resume.
- Gateway streaming, history sequencing, and tool-panel reconstruction reduce lag and stale UI state.
- Recorded HTTP replay works before live DNS resolution without bypassing live-request safety checks.
- Current Wasmtime cache configuration is accepted on Windows.
- The standalone XMPP bridge resolves LunarWing packages consistently at `2.0.0`.

## Source Baseline

- v1 baseline: annotated tag `v1.1.9.0`, dereferenced commit `4ecd58763ad8da477356e1245d32fa4f5d977f31`.
- v2 root commit: `13f5d80ed2440d8cb2791a748512e49d63f76303`; the v1 baseline-to-root imported snapshot changes 150 paths, with 58,802 insertions and 6,849 deletions.
- v2 draft HEAD reviewed for these notes: `b3d53210a2bfc65e10fe18761bb904d40ede3c88`; the native root-to-HEAD range contains 224 later commits.
- The repositories have independent histories, so there is no native `v1.1.9.0..v2` commit range.
- Exact v1 baseline-to-v2 draft-tree comparison: 438 changed paths, 98,470 insertions, and 13,705 deletions.
- Most raw additions are vendored Pebble source and documentation. The release inventory above is based on final behavior and source evidence, not line count or proposal text.
