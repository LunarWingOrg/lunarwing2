# Release Notes for LunarWing v2.1.8.0

## Codename: Umioto (海音)

**Status:** updated draft, not yet released. Release date TBD.

> No flame on the waves,<br>
> yet in the shell, the sea speaks;<br>
> we listen, then sail. 海音

*Hold a shell to your ear, and the sea is still there.*

Shiranui's unseen fire has dimmed. **Umioto listens to the water beneath it.** Engine V2 recovery becomes more truthful, external workers and tenant upgrades fail more safely, and the dependency and worker supply chains move forward. Wingtip and the versioned gateway chat contract add a new way to reach the daemon without disturbing that maintenance current, with stricter listener and TLS handling plus bounded event buffering alongside it.

## Highlights

- **Engine V2 joins and history recover without crossed wires (WI#490, WI#517, WI#518, WI#520, WI#532, WI#534, WI#535).** Thread runs are generation-scoped, terminal outcomes can be recovered after registry loss, and duplicate waiters cannot complete one run twice. Turn-recovery references are per-thread, durable roles share one census, chat history exposes live Engine approvals, and default-project creation is idempotent. Unloadable project records are preserved before recovery replacement.
- **Provider reasoning and its usage are no longer dropped (WI#546).** Streaming and blocking adapters preserve displayable reasoning, and `reasoning_tokens` remains a subset of output-token accounting. Setting `ENGINE_V2_STREAM_REASONING=true` opts gateway browser and TUI clients into coalesced live `reasoning_update` snapshots capped to the trailing 16 KiB; the default is off, and raw provider reasoning is never forwarded to WASM channels, XMPP, or the local REPL.
- **Wingtip brings chat, images, and voice to the terminal (!2173, WI#554).** The Bun/OpenTUI client launches as `wingtip` and supports LunarVoice and optional Hex dictation. The [gateway chat contract](docs/guides/GATEWAY_CHAT_API.md) adds `GET /api/chat/capabilities`, `client_turn_id` correlation, and `turn_started`/`turn_finished` SSE events. Uploads are validated by content. `/quit`, `/exit`, and `/shutdown` are refused on the gateway and unix-socket REPL channels, and a model's silent reply no longer stops the daemon there.
- **Wingtip fails closed on unsafe local and transport configuration (WI#556).** Tenant access grants and local listener-owner checks accept only `localhost` or IP literals, the embedded Hex child must listen on loopback only, and every gateway fetch explicitly requires TLS verification even when the environment tries to disable it. Desktop revocation avoids opening configuration files and reports confirmed client-token revokes accurately. Event queues and pre-connect buffers are bounded with resync notices, while escape-string truncation is visible and covers OpenTUI's OSC/DCS BEL/ST forms.
- **Silent replies no longer stop the daemon on any channel (WI#550).** V1 `NO_REPLY` and V2 completions without response text share the empty-response suppression sentinel, so they finish `no_reply`, emit nothing, and leave the agent ready for the next message. The legacy command adapter can no longer reinterpret suppressed, interrupted, approval-pending, or message-less success results as a shutdown signal.
- **External workers become easier to observe and stop (WI#464, WI#476, WI#519, WI#552).** Boot and periodic liveness probes report dead endpoints without delaying startup, `lunarwing doctor` checks configured workers, and blocking dispatch failures are logged. Cancellation synchronizes runtime, context, and durable state; Pebble forwards progress while execution is still running. A lane-local configuration or image-contract failure no longer prevents later selected lanes from starting.
- **Tenant upgrades build before they stop (WI#507, WI#526, WI#527, WI#551).** Worker candidates are built before the daemon stops with correctly formed image references, promotion retains old image IDs for rollback, stage markers drive convergence after a failure, and inherited upgrade-lock descriptors are scrubbed from service children. Tenant Pebble images build from the verified tenant source rather than an operator's moving checkout.
- **Worker configuration and harness secret handling fail closed (WI#523, WI#529).** OpenCode's immutable baked configuration now has one writable-copy contract for model, Paseo, and Study Notes rewrites. Harness redaction catches raw secrets after line folding, merges overlapping raw and encoded matches without double replacement, handles multibyte boundaries safely, and avoids redacting structural armor lines.
- **Network pinning covers proxies and embedded addresses (WI#503, WI#506, WI#536).** Vetted WASM HTTP requests and built-in skill fetches bypass configured proxies so the connection uses the validated address. Private IPv4 embedded in 6to4 or Teredo is refused consistently, and Pebble's declared model backend is derived into the tenant egress allowlist with a fail-closed start check.
- **The supply chain and worker pins advance together (WI#531).** All eleven LunarWing-owned packages move to `2.1.8`, with registry packages, lockfiles, and vendored crates refreshed and re-audited. Kimi Code moves to `2.4.1`, OpenCode to `v1.19.5`, and OpenCode 2 to `v2.2.5`; the Pebble upstream source ref remains unchanged.
- **Hosted MCP integrations are validated through real Engine V2 turns (WI#562).** A persistent OpenRC/rootless-Podman tenant activated CoinGecko, Exa, and Context7 without credentials and completed real price, web-search, and documentation calls through the gateway. The official Brave Search stdio server installs but requires `BRAVE_API_KEY` or its key-file alternative before activation; no substitute search path is used. The [validation report](docs/ops/WI562_MCP_TESTING.md) records the retained tenant and authenticated follow-up.
- **Worker image builds ride out Debian snapshot outages and scan with a current Trivy (WI#557, WI#558, WI#559, WI#560).** Every worker Dockerfile bounds `apt-get update` to 240 s and package installs to 600 s (1800 s for the optional toolchain layer), and the `worker-kimi-ts` CI apt helper applies the same limits. Protected worker-image builds and that helper retry recognized snapshot download, DNS, and connection failures and hung `apt` runs with bounded backoff; a killed build step is retried only when its own command references `snapshot.debian.org`, so unrelated out-of-memory kills fail at once. The worker-scan gate moves to Trivy `0.76.1`, pinned by digest, which includes the eMudhra CA distrust. Kimi's in-image test gate now tolerates a busy build host, and a dedicated `tui-ts` job keeps Wingtip's TypeScript suite in the validation path.
- **Worker APT transport closes the HTTP retry exposure class (WI#561).** All worker image and CI provisioning sources use HTTPS snapshots with certificate verification intact. The old base-image client installs only `apt` with `Acquire::Retries=0`; after a fixed-version floor passes, the fixed client installs all remaining packages with bounded retries, limiting CVE-2026-109518's hard-link reuse exposure without making tenant builds fragile to one transient fetch. Every update fails on any index error, and the outer retry ladders recognize HTTP and HTTPS traces. Snapshot `20261105T082236Z` supplies fixed APT `2.6.2` on Bookworm and `3.0.4` on Trixie. Trusted CA bundles come from pinned local image inputs and are regenerated after CA installation.

See the [v2.1.7.0 release notes](docs/releases/RELEASE-v2.1.7.0.md) for the previous compatibility baseline.

## Compatibility

| Area | v2.1.8.0 |
|---|---|
| Owned packages / Rust | Eleven packages at `2.1.8`; Rust 2024, MSRV `1.96.1` |
| WASM runtime | Wasmtime/WASI `36.0.20`, Cranelift `0.123.20` |
| Channel registry | darkirc `0.1.14`, multica `0.1.17`, weechat `0.2.15`, xmpp `0.1.20` |
| Tool registry | cavepony-tool `0.4.9`, gitlab `0.1.13`, gotify `0.1.5`, llm-context `0.1.7`, multica-bridge `0.1.8`, ssh `0.1.6`, vision-analyze `0.1.8`, voice-transcribe `0.1.6`, web-search `0.2.8` |
| External workers | Kimi Code `2.4.1`, OpenCode `v1.19.5`, OpenCode 2 `v2.2.5`, Pebble ref `2d0f3786…` |
| Worker image toolchain | Debian snapshot `20261105T082236Z`; APT `>=2.6.2` on Bookworm and `>=3.0.4` on Trixie; Node `22.23.4` (Kimi); Bun `1.4.4` (OpenCode and OpenCode 2) |
| Terminal client | Wingtip `0.1.0`; Bun `>=1.3.14`, OpenTUI `0.8.1`, React `19.3.1` |

Registry bumps require rebuilt WASM artifacts; they do not by themselves change channel protocols. The [v2.1.8.0 cargo-deny audit](docs/ops/v2.1.8.0_CARGO_DENY_AUDIT.md) records the advisory and acceptance baseline.

## Upgrade Notes

1. **Back up and deploy components together.** Preserve the database, tenant state, port registry, workspaces, and channel state. Rebuild and restart the daemon and XMPP bridge, then rebuild and reinstall all four channel and nine tool registry extensions through the normal deployment path.
2. **Rebuild and promote every enabled worker image.** Use the normal candidate, scan, and promotion path, then run `upgrade-tenant` or the appropriate `build-tenant --with-*` command followed by `restart-tenant`. Source updates do not change already-running containers. The new upgrade flow retains pre-promotion images for rollback; do not prune them before the upgrade completes.
3. **Re-declare the Pebble model backend before its next start (WI#506).** Tenants configured with `--with-pebble` before this release may carry no backend key and will be refused at the next Pebble start. Re-run `configure-pebble <tenant>` to pin the backend, or grant it explicitly with `configure-worker-egress <tenant> --allow <backend-host>:443`; then restart and inspect `worker-egress <tenant>`. See the [Pebble worker runbook](docs/ops/PEBBLE-WORKER.md).
4. **Validate the OpenCode v1 lane before broad rollout.** The protected candidate builds and scans remain the image acceptance gate. After the first candidate build, run one live v1 dispatch on a test tenant, then rebuild or restart managed tenants through mt-admin and audit listeners and egress as usual.
5. **Grant Wingtip access deliberately and review gateway upload behavior.** Desktop users need an explicit `tui-access` grant. Daemons without `client_tokens` require the `--use-master-token` stopgap, which grants full gateway access until `rotate-gateway-token <tenant>`; see [Wingtip's README](tui/README.md). Tenant access files and `tui-access --url-host` accept only `localhost` or an IP literal; DNS names are refused there, while remote HTTPS profile URLs are unchanged. Inherited TLS-verification bypasses are refused, and the embedded Hex child must bind to loopback only. Unsupported image types and uploads over the advertised limits now fail with 422/413 instead of being silently dropped.

> Soft rain on the hull,<br>
> small repairs hold back the dark;<br>
> the deep keeps its song.

## Known Limitations

- **The OpenCode v1.19.5 bump is not a proven fix for WI#333.** The model-metadata and silent-stream failure paths observed on v1.18.x are not verified fixed, and the relevant prompt-assembly code remains unchanged. Keep the live-dispatch validation above in the rollout plan.
- **Wingtip currently uses gateway transport only.** Its unix-socket transport is not yet available; legacy daemons use first-in-first-out reply matching and do not deliver generated images. Server-side client-token loading, expiry enforcement, and stream termination remain planned M2b work (section 8.3.5 of the [Wingtip design](docs/proposals/WINGTIP_DESIGN.md)); until then, token expiry is honoured by the TUI alone.
- **Keep live provider reasoning off on shared gateways (WI#546).** Until routine-originated Engine V2 turns carry user scope, statuses without that metadata use the existing broadcast path to all subscribers, including live reasoning when enabled. Keep `ENGINE_V2_STREAM_REASONING=false` there; snapshots are live-only and are not stored in durable history.
- **Worker isolation keeps its documented boundary.** The bridge/task split protects hub credentials, but jobs inside one task container remain one authority domain. Tool grants stay empty by default, and worker egress remains allowlist-based. The [v2.1.7.0 deployment boundaries](docs/releases/RELEASE-v2.1.7.0.md#known-limitations) apply where not superseded here.
- **Kimi's image build can still be load-sensitive.** WI#560 widened the in-image test timing budgets and isolated the tool-proxy tests, but `lunarwing_bridge_test.js` has been seen to hit the 120 s test timeout on a heavily loaded build host. Its cause is not yet known; retry the candidate build on a quieter host if it recurs.
- **APT hardening still requires image acceptance and has no independent second-host hash check (WI#561).** Lane C's second-host SHA-256 verification is not implemented. The fixed APT floors are satisfied by the pinned security suites; future snapshot refreshes must confirm those floors remain available before building. Protected candidate/rebuild digest comparisons, all four scan jobs, and the opencode2 exposure verifier remain mandatory rollout gates.

---

> Dawn without a blaze,<br>
> the tide answers in low tones;<br>
> we carry the sound.

海音
