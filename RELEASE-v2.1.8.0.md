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
- **Wingtip brings chat, images, and voice to the terminal (!2173, WI#554).** The Bun/OpenTUI client launches as `wingtip` and supports LunarVoice and optional Hex dictation. The [gateway chat contract](docs/guides/GATEWAY_CHAT_API.md) adds `GET /api/chat/capabilities`, `client_turn_id` correlation, and `turn_started`/`turn_finished` SSE events. Uploads are validated by content. `/quit`, `/exit`, and `/shutdown` are refused on the gateway and unix-socket REPL channels, and a model's silent reply no longer stops the daemon there.
- **Wingtip fails closed on unsafe local and transport configuration (WI#556).** Gateway access grants and listener-owner checks refuse DNS-name matches, wildcard Hex binds are rejected, and every gateway fetch explicitly requires TLS verification even when the environment tries to disable it. Desktop revocation avoids opening configuration files and reports confirmed client-token revokes accurately. Event queues and pre-connect buffers are bounded with resync notices, while escape-string truncation is visible and covers OpenTUI's OSC/DCS BEL/ST forms.
- **Silent replies no longer stop the daemon on any channel (WI#550).** V1 `NO_REPLY` and V2 completions without response text share the empty-response suppression sentinel, so they finish `no_reply`, emit nothing, and leave the agent ready for the next message. The legacy command adapter can no longer reinterpret suppressed, interrupted, approval-pending, or message-less success results as a shutdown signal.
- **External workers become easier to observe and stop (WI#464, WI#476, WI#519, WI#552).** Boot and periodic liveness probes report dead endpoints without delaying startup, `lunarwing doctor` checks configured workers, and blocking dispatch failures are logged. Cancellation synchronizes runtime, context, and durable state; Pebble forwards progress while execution is still running. A lane-local configuration or image-contract failure no longer prevents later selected lanes from starting.
- **Tenant upgrades build before they stop (WI#507, WI#526, WI#527, WI#551).** Worker candidates are built before the daemon stops with correctly formed image references, promotion retains old image IDs for rollback, stage markers drive convergence after a failure, and inherited upgrade-lock descriptors are scrubbed from service children. Tenant Pebble images build from the verified tenant source rather than an operator's moving checkout.
- **Worker configuration and harness secret handling fail closed (WI#523, WI#529).** OpenCode's immutable baked configuration now has one writable-copy contract for model, Paseo, and Study Notes rewrites. Harness redaction catches raw secrets after line folding, merges overlapping raw and encoded matches without double replacement, handles multibyte boundaries safely, and avoids redacting structural armor lines.
- **Network pinning covers proxies and embedded addresses (WI#503, WI#506, WI#536).** Vetted WASM HTTP requests and built-in skill fetches bypass configured proxies so the connection uses the validated address. Private IPv4 embedded in 6to4 or Teredo is refused consistently, and Pebble's declared model backend is derived into the tenant egress allowlist with a fail-closed start check.
- **The supply chain and worker pins advance together (WI#531).** All eleven LunarWing-owned packages move to `2.1.8`, with registry packages, lockfiles, and vendored crates refreshed and re-audited. Kimi Code moves to `2.4.1`, OpenCode to `v1.19.5`, and OpenCode 2 to `v2.2.5`; the Pebble upstream source ref remains unchanged.
- **CI absorbs transient Debian snapshot failures more gracefully (WI#557).** Protected worker-image builds and the `worker-kimi-ts` apt provisioning step retry recognized snapshot download, DNS, and connection failures with bounded backoff instead of failing on the first transient outage. A dedicated `tui-ts` job also keeps Wingtip's TypeScript suite in the validation path.

See the [v2.1.7.0 release notes](docs/releases/RELEASE-v2.1.7.0.md) for the previous compatibility baseline.

## Compatibility

| Area | v2.1.8.0 |
|---|---|
| Owned packages / Rust | Eleven packages at `2.1.8`; Rust 2024, MSRV `1.96.1` |
| WASM runtime | Wasmtime/WASI `36.0.20`, Cranelift `0.123.20` |
| Channel registry | darkirc `0.1.14`, multica `0.1.17`, weechat `0.2.15`, xmpp `0.1.20` |
| Tool registry | cavepony-tool `0.4.9`, gitlab `0.1.13`, gotify `0.1.5`, llm-context `0.1.7`, multica-bridge `0.1.8`, ssh `0.1.6`, vision-analyze `0.1.8`, voice-transcribe `0.1.6`, web-search `0.2.8` |
| External workers | Kimi Code `2.4.1`, OpenCode `v1.19.5`, OpenCode 2 `v2.2.5`, Pebble ref `2d0f3786…` |
| Terminal client | Wingtip `0.1.0`; Bun `>=1.3.14`, OpenTUI `0.8.1`, React `19.3.1` |

Registry bumps require rebuilt WASM artifacts; they do not by themselves change channel protocols. The [v2.1.8.0 cargo-deny audit](docs/ops/v2.1.8.0_CARGO_DENY_AUDIT.md) records the advisory and acceptance baseline.

## Upgrade Notes

1. **Back up and deploy components together.** Preserve the database, tenant state, port registry, workspaces, and channel state. Rebuild and restart the daemon and XMPP bridge, then rebuild and reinstall all four channel and nine tool registry extensions through the normal deployment path.
2. **Rebuild and promote every enabled worker image.** Use the normal candidate, scan, and promotion path, then run `upgrade-tenant` or the appropriate `build-tenant --with-*` command followed by `restart-tenant`. Source updates do not change already-running containers. The new upgrade flow retains pre-promotion images for rollback; do not prune them before the upgrade completes.
3. **Re-declare the Pebble model backend before its next start (WI#506).** Tenants configured with `--with-pebble` before this release may carry no backend key and will be refused at the next Pebble start. Re-run `configure-pebble <tenant>` to pin the backend, or grant it explicitly with `configure-worker-egress <tenant> --allow <backend-host>:443`; then restart and inspect `worker-egress <tenant>`. See the [Pebble worker runbook](docs/ops/PEBBLE-WORKER.md).
4. **Validate the OpenCode v1 lane before broad rollout.** The protected candidate builds and scans remain the image acceptance gate. After the first candidate build, run one live v1 dispatch on a test tenant, then rebuild or restart managed tenants through mt-admin and audit listeners and egress as usual.
5. **Grant Wingtip access deliberately and review gateway upload behavior.** Desktop users need an explicit `tui-access` grant. Daemons without `client_tokens` require the `--use-master-token` stopgap, which grants full gateway access until `rotate-gateway-token <tenant>`; see [Wingtip's README](tui/README.md). Gateway URLs must use literal IP addresses rather than DNS names, and inherited TLS-verification bypasses or wildcard Hex listeners are refused. Unsupported image types and uploads over the advertised limits now fail with 422/413 instead of being silently dropped.

> Soft rain on the hull,<br>
> small repairs hold back the dark;<br>
> the deep keeps its song.

## Known Limitations

- **The OpenCode v1.19.5 bump is not a proven fix for WI#333.** The model-metadata and silent-stream failure paths observed on v1.18.x are not verified fixed, and the relevant prompt-assembly code remains unchanged. Keep the live-dispatch validation above in the rollout plan.
- **Wingtip currently uses gateway transport only.** Its unix-socket transport is not yet available; legacy daemons use first-in-first-out reply matching and do not deliver generated images. Server-side client-token loading, expiry enforcement, and stream termination remain M2b follow-up work (WI#556).
- **Reasoning output remains invisible across provider paths (WI#546).** Streaming and blocking adapters drop real model reasoning before it reaches the engine or gateway, reasoning-token usage is not accounted for, and the gateway's `reasoning_update` event still carries tool-status text rather than model reasoning. Thinking-style models can therefore appear idle until final content arrives.
- **Worker isolation keeps its documented boundary.** The bridge/task split protects hub credentials, but jobs inside one task container remain one authority domain. Tool grants stay empty by default, and worker egress remains allowlist-based. The [v2.1.7.0 deployment boundaries](docs/releases/RELEASE-v2.1.7.0.md#known-limitations) apply where not superseded here.
- **Release tooling has follow-ups (WI#558, WI#559, WI#560).** Worker scans still use Trivy `0.74.0` pending the verified `0.76.1` pin. Snapshot retries do not bound hung `apt` downloads, and build-log exit-137 matching remains too broad. Kimi's in-image tests can also be timing-sensitive under CPU load.

---

> Dawn without a blaze,<br>
> the tide answers in low tones;<br>
> we carry the sound.

海音
