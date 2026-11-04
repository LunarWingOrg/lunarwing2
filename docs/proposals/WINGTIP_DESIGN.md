# Wingtip, the LunarWing multimodal TUI: final design

Status: final design, 2026-11-03. Revised from the integration draft (`design/DESIGN_DRAFT.md`) after the security (SEC-01 to SEC-14), feasibility (F1 to F15) and UX (UX-01 to UX-15) reviews; the disposition of every finding is in Appendix B. Target: v2.1.9.0 roadmap item "new interactive TUI" (goal 35 in `docs/ops/AGENT_GOALS_2.1.8.0.md:315`).

> **Repository copy (added 2026-11-03).** This is the design that the Wingtip client (`tui/`) and its daemon and `mt-admin tui-access` changes were built from. Code comments cite its sections (for example SEC-01, 3.8.1, 8.3.5 and 11.4), and the MR !2173 review (finding UZ-11) asked for it to be in the repository. The text is as written, except for these edits:
>
> - **Names.** The client was renamed Wingtip after this design was written (WI#554). Paths and identifiers below use the new names: the launcher `tui/bin/wingtip`, the package `wingtip`, `~/.config/wingtip/`, `~/.local/state/wingtip/`, `$XDG_RUNTIME_DIR/wingtip/` and the access-file schema `wingtip/tenant-access`. Environment variables keep their `LUNARWING_TUI_` prefix, as in the code.
> - **Research citations.** `<research>/` stands for the research workspace the design was written from: the brief, the area designs, the cross-check reports, the unpacked package sources and the probe scripts. It is not part of the repository. Where a fact from it matters, it is quoted here.
> - **Paths.** The `cd ic` and `cd tui` commands run from the repository root. `ic/...:N` line citations refer to commit `724ea0a05` and drift on later commits.
>
> **Status on master (2026-11-03):**
> - **Merged** (!2173): M1a, M1b and M2a.
> - **Since then:** a default `tui-access` grant refuses unless the running daemon lists the `client_tokens` capability feature, and `rotate-gateway-token` rotates `GATEWAY_AUTH_TOKEN` (3e495eb61).
> - **Not built yet:** M2b (client tokens, 8.3.5), M3 (socket hardening, WI#458) and M4 (REPLv3 and the client socket transport). Their sections describe planned work.
> - **Open review findings:** WI#556 (UZ-3 to UZ-9) and WI#550 (silent replies still stop the daemon on channels other than the gateway and the socket).
> - **Appendix A decisions:**
>   - DEC-1: adopted, implemented in M2a.
>   - DEC-2: changed. The live tests used the existing test tenant `dwarfstar`, upgraded in place.
>   - DEC-3 and DEC-4: took the recommended option (a).
>   - DEC-5 and DEC-6: as recorded.

Inputs, in order of authority:
1. `<research>/BRIEF.md` (Christopher's binding decisions and verified facts).
2. `<research>/reports/crosscheck_lunarwing_side.md` and `crosscheck_external_side.md` (win over the other reports).
3. The three area designs: `design/area-client.md`, `design/area-gateway.md`, `design/area-replv3.md`.
4. The source at branch `tuitui3`, HEAD `724ea0a05` (this repository), checked read-only for every point where the area designs disagree and for every review finding.
5. The three reviews of the draft; their findings are resolved in Appendix B.

This document supersedes the three area designs and the draft where they differ. Where it says "as specified in the area design", the area text applies unchanged; everything an implementer needs is nevertheless stated here.

---

## 0. Reading guide and integration decisions

### 0.1 Citation conventions

| Prefix | Meaning |
|---|---|
| `ic/...:N` | file and line in the working tree at `724ea0a05` |
| `v4:...` | `git show origin/vibetui4:<path>`; byte-identical copy at `<research>/vibetui4-tui/tui` |
| `ot081:...` | `@opentui/core` 0.8.1 at `<research>/design/pkgs/x-opentui-core-0.8.1/package/` |
| `otr081:...` | `@opentui/react` 0.8.1 at `<research>/design/pkgs/x-opentui-react-0.8.1/package/` |
| `hex:...` | Hex source at `<research>/hex-src` (dev 54a3a66, Cargo 3.3.0) |
| `hexbin:` | the released `hex-3.3.0-linux-x64` binary unpacked at `<research>/hex-bin/extract/usr/bin/hex` (inspected with `strings` only) |
| `probe:...` | loopback-only Bun 1.3.14 experiments at `<research>/design/scripts/` (results quoted where cited) |
| **NEW** | a type, field, event, route or file this design adds |
| IDs | `D1`-`D12` daemon defects and `C1`-`C12` client defects from the vibetui4 audit; `DEC-1`-`DEC-6` decisions for Christopher (Appendix A.0); `SEC-nn`, `Fn`, `UX-nn` review findings (Appendix B); `R1`-`R31` area-design resolutions (0.3); `G1`-`G8` gateway-area work items |

Line numbers refer to HEAD and shift as earlier milestones land. Implementers locate code by function name; the line is a pointer.

### 0.2 Decisions at a glance

| # | Decision |
|---|---|
| I1 | Two transports behind one client `Transport` interface: the gateway (HTTP POST + SSE, bearer token) and a Unix-socket protocol named **REPLv3** (wire protocol number 3). vibetui4's "protocol v2" name and shape are dropped. |
| I2 | One server-side **message lifecycle hook**, `Channel::message_lifecycle(msg, Started \| Finished(outcome))`, called by the agent loop for every inbound message on every engine. The gateway maps it to NEW SSE events `turn_started` / `turn_finished`; REPLv3 maps it to `TurnStarted` / `TurnFinished` frames. This replaces the gateway area's V2-only `turn_finished` (its G2). |
| I3 | One outcome vocabulary on both transports: `replied`, `no_reply`, `paused`, `interrupted`, `detached`, `failed`, `timed_out`, `cancelled` (Rust enum `lunarwing_common::TurnOutcome`). On both transports the reply for a message is always delivered before its `turn_finished`. |
| I4 | One correlation rule: the client generates a UUIDv4 `TurnId` per user turn and per control; the gateway calls it `client_turn_id`, REPLv3 calls it `msg_id`. Both servers validate `^[A-Za-z0-9_-]{1,64}$` with one shared function and echo the id on every event caused by that message. An approval continuation is correlated to the **original** turn on both transports. |
| I5 | One capability model: both servers advertise the same `features` names (`status`, `attachments`, `transcribe`, `stream`) and the same `limits` object (`lunarwing_common::WireLimits`). The gateway serves it at NEW `GET /api/chat/capabilities`; REPLv3 sends it in `Welcome`. A 404 or a v1 welcome means "legacy daemon", and the client degrades. |
| I6 | Voice: dictate, edit, Enter. Server STT is LunarVoice only, on both transports (gateway `/api/voice/transcribe`; REPLv3 `Transcribe` through the same `VoiceApi`). Hex is the local fallback through `POST /transcriptions` of the running app or of `hex service --embedded` (M1b); Hex `/dictations` and the Hex CLI are deferred (13, Deferred). The TUI always records; a failed clip is kept in memory for one retry. Audio is never sent as a chat attachment and never persisted. |
| I7 | Images: PNG, JPEG, GIF and WebP only, sniffed from magic bytes on the client and on both servers by one shared Rust helper (`ic/src/channels/image_mime.rs`). Engine V2 gains `ImageGenerated`. |
| I8 | `/quit`, `/exit`, `/shutdown` are intercepted by the client on every transport and refused by the daemon for the channels `gateway` and `unix_socket_repl` at the `Submission::Quit` arm. With decision DEC-1 (recommended), an `Ok(None)` result from those two channels never stops the daemon either (8.4). |
| I9 | Socket turns stay on Engine V1 (no token streaming). Streaming comes from the gateway transport. Making the socket V2-eligible is a later, opt-in change with no wire change. |
| I10 | Multi-tenant hosts: the desktop user reaches tenants through the gateway only, provisioned by NEW `lunarwing-mt-admin.sh tui-access`, with NEW per-user, route-scoped, revocable client tokens. Sockets are for same-uid daemons only. Before any token leaves the client for a loopback TCP port (gateway or Hex), the owner uid of the listening socket is checked against `/proc/net/tcp` (3.8.1). |
| I11 | Client stack: Bun + OpenTUI + React 19, salvaging vibetui4; OpenTUI pinned exactly to 0.8.1. |
| I12 | Delivery order: M1a client gateway slice with LunarVoice, images and `tui-access` against today's daemons (first usable build, an internal checkpoint); M1b Hex sources, so the first client release (M1a plus M1b) has both voice backends, as decision 2 requires; M2a gateway contract in the daemon (lifecycle, correlation, quit refusal, image validation, V2 images, voice exposure); M2b client tokens; M3 socket hardening on protocol 1; M4 REPLv3 plus the client socket transport. |
| I13 | Composer: a word-wrapping multi-line `<textarea>` (1 to 6 rows) where Enter sends and Ctrl+J, Alt+Enter or Shift+Enter (KKP) insert a newline; clears are undoable and recallable (3.10.5). |
| I14 | Discoverability: F1 and `/help` open an overlay info panel that also holds voice status, privacy, devices, transport, threads and full approval text; a one-line footer always shows the keys that apply to the current state (3.12.3). |

### 0.3 Resolution log (inconsistencies between the area designs)

Each entry names the conflict, the resolution, and the reason. Section references point to where the resolution is specified.

| ID | Conflict | Resolution | Reason |
|---|---|---|---|
| R1 | Turn completion: gateway area emits `turn_finished` from `TurnCompletion::complete` (V2, gateway only, before the response); REPLv3 area emits `TurnFinished` from a new agent-loop hook (after the response). | One hook, `Channel::message_lifecycle`, called from the agent loop (`ic/src/agent/agent_loop.rs:1326-1427`) plus the V2 late watcher. Both transports map it. G2's `outcome.rs` change is dropped. (8.2) | The V2-only emitter misses V1 gateways (`ENGINE_V2=false`), V2 controls, commands that fall through to V1, and early `Err` returns. The hook sees every message exactly once. |
| R2 | Event order: gateway `turn_finished` then `response`; REPLv3 `Response` then `TurnFinished`. | Reply first, then `turn_finished`, on both transports. The client still tolerates either order. (4.1, 3.5) | The hook fires after `deliver_message_result` has handed the reply to `respond()` (`agent_loop.rs:1393-1427`), on the same task, so the order is guaranteed. |
| R3 | Outcome names: gateway `completed/failed/max_iterations/stopped/paused/error` + `response_expected`; REPLv3 `replied/no_reply/awaiting_approval/failed/timed_out/cancelled`; client `FinishOutcome`. | One enum `TurnOutcome` with `replied`, `no_reply`, `paused`, `interrupted`, `detached`, `failed`, `timed_out`, `cancelled` (+ `unknown` for forward compatibility). `response_expected` is dropped. (4.1) | R2 makes `response_expected` redundant. `paused` covers approval, credential and external gates. `interrupted` and `detached` are needed so V1 and V2 report the same thing. |
| R4 | Refined outcomes: REPLv3 refines `NoReply` inside the channel (`approval_pending`); the gateway had no equivalent. | Outcome hints recorded by the code that knows (V1 result arms, V2 completion, V2 detach) in a bounded map on `ChannelManager`, applied once when `Finished` is reported. (8.2) | One place, both channels, both engines. Channels stay thin. |
| R5 | Correlation id charset: gateway `[A-Za-z0-9_-]{1,64}`, REPLv3 `[A-Za-z0-9._:-]{1,64}`. | `^[A-Za-z0-9_-]{1,64}$` everywhere, via `lunarwing_common::client_caps::is_valid_client_id`. (4.1) | UUIDs and the REPLv3 examples (`c-0007`, `t-0003`, `s-<uuid32>`) fit. One validator, one TypeScript mirror. |
| R6 | Correlation field names: `client_turn_id` (gateway) vs `msg_id` (REPLv3). | Kept per transport; same value (the client `TurnId`), same echo rules, same approval semantics. (4.1) | Both names are fixed in their area specs and fixtures; renaming buys nothing. |
| R7 | Approval correlation: the gateway client sends the original `client_turn_id` in the approval POST; REPLv3 looks up the original `msg_id` in a per-thread pending map. | Both stay; the rule is "continuation events carry the original turn's id". REPLv3 approvals carry their own control `msg_id` for the `Ack`. (4.2.7, 4.3.4, 8.6.2) | The gateway has no per-connection state; the socket does. The client sees the same semantics. |
| R8 | `/quit` refusal: gateway area allow-list (`channel == "repl"`), REPLv3 area deny-list (`unix_socket_repl` only). | Deny-list `["gateway", "unix_socket_repl"]` at `agent_loop.rs:2151`. An allow-list for all channels is open question A.11. (8.4) | Covers both TUI transports without changing XMPP, Signal, HTTP or WASM behaviour (scope). |
| R9 | Media sniffing: `channels/image_mime.rs` (images) vs `channels/media_sniff.rs` (images and audio). | One `ic/src/channels/image_mime.rs`, images only, with one `decode_images()` used by the gateway and the socket. (6.2, 8.3.4) | Audio sniffing is not needed once socket `Transcribe` goes through `VoiceApi` (R11), which forwards to the sidecar like the gateway. |
| R10 | Attachment ids: `web-image-<uuid>` vs bare UUID vs `repl-attachment-<uuid>`. | `<prefix>-<uuid v4>` from `decode_images()`, prefixes `web-image` and `repl-image`. (8.8) | One pattern, readable store paths. |
| R11 | Socket transcription backend: gateway area wanted `VoiceApi` (LunarVoice), REPLv3 area chose `TranscriptionProvider` (LunarVoice, OpenAI, chat_completions). | `VoiceApi` (LunarVoice only). Dropped from scope: `TranscriptionConfig::create_shared_provider`, `TranscriptionMiddleware::from_shared`, `TranscriptionError::Unavailable`. (5.3, 8.6.3) | Brief decision 2(a) names LunarVoice for both transports. It keeps desktop audio on the host (no silent cloud upload, the gateway area's D10), and it does not depend on `TRANSCRIPTION_ENABLED`. |
| R12 | Transcription concurrency: REPLv3 global `Semaphore(1)` with a 30 s queue; gateway none. | One transcription in flight per connection, no global permit; sidecar 503 maps to `busy` (retryable). (4.3.7) | The sidecar already admits one call per component; a second queue only hides the busy signal. |
| R13 | Capabilities shape: gateway booleans (`ChatCapabilities`) vs REPLv3 `Welcome.features` + `limits`. | Shared `features` names and shared `WireLimits` struct for both. (4.1, 4.2.2) | One client decoder and one capability model. |
| R14 | Desktop config layout: `tenants/<t>.json {url, token}` (gateway) vs `config.toml` profiles + `tokens/<profile>.token` + URL derived from `ports.json` (client). | Tenants: only `tenants/<t>.json` written by `tui-access`. `config.toml` and `tokens/<profile>.token` only for non-tenant (single-node, dev) profiles. The client never reads the ports registry. One copy of each secret. (3.11, 9.5) | The file's `url` handles tenants bound to a specific address; mt-admin owns registry knowledge. |
| R15 | Client-token route scope vs credential mode: the client asked to allow `/api/chat/auth-token` and `/api/chat/auth-cancel`. | Covered by the allow-list `/api/chat/*` except `/api/chat/ws`; with a client token, `auth-token` additionally requires a pending authentication request for that extension (SEC-07, 8.3.5). | Verified routes `ic/src/channels/web/server.rs:468-469`; the handler configures any extension today (`:1625-1645`). |
| R16 | Client token path configuration: gateway area added `GatewayConfig.client_tokens_file`, which touches 14 struct literals in 8 files. | No `GatewayConfig` field. `client_tokens::store_from_env()` resolves `GATEWAY_CLIENT_TOKENS_FILE` with the config layer's `optional_env` (`ic/src/config/helpers.rs:209`) and refuses paths outside the sandbox-protected directories; `main.rs` only calls it and `GatewayChannel::with_client_tokens`. (8.3.5; confirm in A.18) | Keeps parallel tasks on disjoint files (the literals sit in `bridge/router.rs` and `bridge/effect_adapter.rs`, which another M2a task owns); tests never pick up a stray file. |
| R17 | `/restart`: client forwards after confirmation; gateway area says intercept locally. | Refused locally with guidance (`mt-admin restart-tenant`). `//restart` forwards after a second Enter. (3.10) | Restarting a daemon is an operator action; the daemon refuses it outside Docker anyway (`ic/src/agent/commands.rs:574-588`). |
| R18 | Busy retry: client "fall back at once", gateway "retry 1, 2, 4 s then Hex". | Retry the same source after 1, 2 and 4 s (3 retries, inside the 180 s budget), then the next candidate; when every candidate fails the clip is kept for Alt+R (5.4.1). (5.4) | LunarVoice busy windows are short (one admitted call); switching models for a transient condition is worse. |
| R19 | Legacy client sharing: REPLv3 area moves `cli/repl.rs` and `unix-socket-client` onto shared types (path dependency for the standalone crate). | Both keep their own private v1 types; each gains a test pinning its encodings to the golden v1 fixtures, and CI runs the standalone crate's tests (F11). `cli/repl.rs` also refuses a socket owned by another uid before connecting (SEC-11). (8.5.6) | No wire change and no lockfile change in legacy clients; drift is caught for both. |
| R20 | `max_content_chars`: client assumed 32000; REPLv3 uses 100000 bytes. | Both servers advertise `limits.max_content_bytes = 100000`. (4.1) | The safety validator's default maximum is 100 000 (`ic/crates/lunarwing_safety/src/validator.rs:93`), which applies to both channels. |
| R21 | `turn_finished` on V1 gateways (client open request). | Yes: the hook is engine-agnostic. | R1. |
| R22 | Bare approval keyword with nothing pending on the gateway: gateway area proposed a 15 s "consumed as control" heuristic. | Current gateways emit `turn_finished{no_reply}` at once (the keyword returns `ok_with_message("")`, `ic/src/agent/thread_ops.rs:1411-1419`); the 15 s heuristic applies only to legacy gateways. REPLv3 answers `Error{no_pending_approval}`. (3.5) | R1. |
| R23 | V2 `Completed{response:None}` returns `Ok(None)` and so shuts the daemon down (`crates/lunarwing_engine/src/executor/orchestrator.rs:2748-2753`, `ic/src/bridge/router.rs:155`, `agent_loop.rs:1412`); gateway area proposed a one-line fix. | Out of scope here, filed separately like the V1 `NO_REPLY` bug (decision DEC-6; risk table in 15). Decision DEC-1 guards the shutdown decision for the two TUI channels (8.4). | Model-triggered, affects every gateway client equally; same bug class the brief keeps out of scope. |
| R24 | Delivery split: REPLv3 area MRs A/B/C; gateway area G1-G8. | Milestones M1a, M1b, M2a, M2b, M3 and M4 (section 13). The lifecycle hook and the quit refusal move to M2a (shared prerequisite). | Usable slice first; disjoint file ownership per milestone. |
| R25 | Socket thread token, client format `tui-<profile>-<16 hex>`, server `^[A-Za-z0-9_-]{1,64}$`. | Profile names restricted to `^[a-z0-9_-]{1,32}$` so the token always validates. (3.11) | Avoids a fatal `bad_frame` on handshake. |
| R26 | REPLv3 `TranscribeResult` had no `language`; the client type has one. | `TranscribeResult{req_id, text, language?, elapsed_ms, model?}`, filled from the LunarVoice response. (4.3.5) | R11 provides it. |
| R27 | Gateway total image budget: client computed `min(count*max, body*3/4)`; gateway area capped count and per-image only. | Gateway advertises `max_total_attachment_bytes = 7 MiB` and enforces it in `decode_images()`; socket advertises 10 MiB. (4.1) | The 10 MiB JSON body limit (`server.rs:702`) caps decoded images near 7.5 MiB; advertising the real cap removes client arithmetic. |
| R28 | Gateway `turn_started`: not in either design. | NEW SSE `turn_started{thread_id}` from the same hook. | Same client mapping as REPLv3 `TurnStarted` (approval continuations, queued turns). |
| R29 | Frame names: the client area's first proposal (`request_id`, `Approve{approval_id}`, `ApprovalNeeded`, `Ack{state, position}`, `TurnFinished{reason}`, `Transcribe{mime_type, data_base64, language}`, negotiated `approvals`/`turn_ids` features) vs the REPLv3 names. | The REPLv3 names of 4.3 everywhere, including the TypeScript mirror and the golden fixtures; the client area's later text already follows them (crosswalk in `design/area-replv3.md` 2.11). | One naming scheme; the server owns the spelling. |
| R30 | Voice order per transport: gateway area "LunarVoice first on a voiced gateway, Hex first on the socket"; client area one fixed order. | One order, `lunarvoice, hex-app, hex-embedded`, on both transports; `lunarvoice` is skipped when the transport does not advertise `transcribe` or is not connected. (5.4) | With R11 the socket's server STT is the same LunarVoice; an unavailable source costs nothing. |
| R31 | Socket `Transcribe` availability: REPLv3 area required `TRANSCRIPTION_ENABLED` plus a provider key; the gateway's voice routes do not. | Advertised exactly when `VoiceApi` exists (`TRANSCRIPTION_PROVIDER=lunarvoice` and `VOICE_AUTH_TOKEN`), the same condition as `/api/voice/*`. (4.3.3) | Same capability on both transports for the same daemon. |

### 0.4 Out of scope

- Fixing the producers of the V1 `NO_REPLY` → Shutdown bug (`agent_loop.rs:2183-2186` → `:1412`; the check applies to every V1 reply, not only group chats) and of the V2 `Completed{response:None}` → Shutdown path (R23). Both stay tracked separately. Decision DEC-1 only guards the shutdown decision for the two TUI channels (8.4).
- Token streaming over the socket (I9), TTS playback (`Audio.playStream` exists but is not used), live ASR partials, auto-send after dictation.
- Hex `/dictations` and the Hex CLI as voice sources (13, Deferred).
- Cross-uid socket brokering for multi-tenant hosts, rebinding tenant gateways from `0.0.0.0`, an mt-admin verb to rotate `GATEWAY_AUTH_TOKEN`.
- Audio and document attachments over REPLv3 (they would run inline on the main loop, `agent_loop.rs:1373-1385`).
- Extending LunarVision enrichment beyond XMPP (`vision_enrichment.rs:85-88`).
- Changes to the web UI (`ic/src/channels/web/static/app.js`); it ignores the new SSE event names.

---

## 1. Goals and non-goals

### 1.1 Goals

1. A new terminal client (`tui/`, Bun + OpenTUI + React 19) that accepts voice and images and renders streaming text, tool progress, approvals and inline generated images.
2. One client interface over two transports: the web gateway (the default and the multi-tenant path) and a hardened Unix-socket protocol, REPLv3 (same-uid daemons).
3. Exact correlation of every reply, progress event, approval and terminal state to the turn that caused it (fixes vibetui4 C1, C2 and D3).
4. Voice from day one with LunarVoice and Hex (`/transcriptions` of the app or the embedded service) and automatic fallback; the transcript lands in a multi-line composer for editing; a failed transcription keeps the recording for one retry.
5. Image input by clipboard, file path, drag-and-drop and region screenshot, validated identically on client and server; generated images on both engines.
6. Push-to-talk that works inside tmux (toggle, mouse hold, repeat-inferred hold) and in kitty directly (key-release hold).
7. Close the socket hazards: WI#458 items 1-6 and defects D1-D12, the `/quit` footgun, cross-window approvals and silent bare keywords.
8. A supported way for the desktop user to reach tenant daemons without reading tenant files: `mt-admin tui-access` plus revocable client tokens.
9. Legacy clients (`lunarwing repl`, `ic/unix-socket-client`) keep working byte for byte; new clients degrade cleanly against old daemons.
10. Every voice, approval, credential and connection state is visible on screen, and the keys that apply are always shown (footer, F1 help).

### 1.2 Non-goals

Everything in 0.4, plus: replacing the web UI, a ratatui client, new vendored crates, new TCP listeners (the ports registry stays at v17), and changing the scheduler's global exclusivity for non-gateway channels (`ic/src/agent/dispatch.rs:26-34`, `:169-172`).

### 1.3 Constraints carried from the brief

- Cargo only as `taskset -c 0-11 cargo check|test|clippy -j12`; never a debug `cargo build`; tmux for long runs.
- Rust 2024, no `unwrap`/`expect` in production, `thiserror`/`anyhow`, `tracing`, functions under 100 lines, cognitive complexity under 15, fewer than 7 arguments, `secrecy` for secrets.
- `ic/src/main.rs` and `ic/src/app.rs` stay orchestration-only. All tenant access goes through `ic/scripts/lunarwing-mt-admin.sh`; systemd user units and OpenRC are both first-class.
- Docs and narrowest tests land with each change.

---

## 2. Architecture

### 2.1 Component diagram

```
 desktop session (uid sun, /run/user/1000: microphone, clipboard, Wayland)
+------------------------------------------------------------------------------+
| wingtip  (Bun, OpenTUI 0.8.1, React 19)                                      |
|                                                                              |
|  keys / mouse / paste --> controller/* (session, commands, keymap)           |
|                               | intents               ^ actions              |
|                               v                       |                      |
|  components/* <-- store <-- reducer (pure) <-- coalesce <-- Supervisor       |
|                                                               ^ TransportEvent (one model)
|  voice/manager (push-to-talk state machine)                   |              |
|    recorder: OpenTUI Audio 16 kHz, or parec                   |              |
|    sources:  lunarvoice  --> Transport.transcribe()           |              |
|              hex-app | hex-embedded (M1b) --> Hex, 127.0.0.1 only, listener uid checked
|  images/* (sniff, clipboard, /img, drag-and-drop, /shot)      |              |
|                                                               |              |
|  Transport (one interface) -----------------------------------+              |
|    GatewayTransport  HTTP POST + SSE, bearer token,     --> [A]              |
|                      listener uid checked first (3.8.1)                      |
|    SocketTransport   REPLv3 or v1 (M4), same uid only   --> [B]              |
|  config: tenants/<t>.json (0600), written by `mt-admin tui-access` as root   |
+------------------------------------------------------------------------------+

 tenant uid (gateway only) or a same-uid single-node / dev daemon (gateway and socket)
+------------------------------------------------------------------------------+
| lunarwing daemon                                                             |
|  [A] GatewayChannel (axum, loopback URL): POST /api/chat/send, /approval,    |
|      /auth-token, /auth-cancel; GET /api/chat/events (SSE), /history,        |
|      /threads, /pending-gates, /capabilities (NEW); /api/voice/*             |
|  [B] UnixSocketReplChannel: socket 0600, SO_PEERCRED same-uid gate,          |
|      bounded reader, per-connection queue + writer task (M3), REPLv3 (M4)    |
|  VoiceApi (one instance, both channels) --> LunarVoice sidecar (loopback)    |
|                                                                              |
|  channels --> msg_tx --> scheduler --> supervise_message (agent loop)        |
|                                          V1 thread_ops | V2 bridge           |
|  ChannelManager: respond / send_status / message_lifecycle (NEW),            |
|                  outcome hints (NEW)                                         |
|    --> GatewayChannel --> SseManager (correlated broadcast, NEW)             |
|    --> UnixSocketReplChannel --> Outbound queue --> socket                   |
+------------------------------------------------------------------------------+
```

### 2.2 One turn, end to end

Gateway (M2a contract):

```
client: TurnId T = uuidv4; record T as pending; POST /api/chat/send {content, thread_id, images?, client_turn_id:T}
daemon: chat_send_handler -> IncomingMessage(gateway) metadata {user_id, thread_id, message_id:M, client_turn_id:T}
        -> scheduler -> supervise_message:
             lifecycle Started            -> SSE turn_started   {thread_id, message_id:M, client_turn_id:T}
             V1 thread_ops | V2 bridge    -> SSE thinking / tool_* / stream_chunk / image_generated / approval_needed ... (+M, T)
             deliver_message_result       -> SSE response       {content, thread_id, M, T}
             lifecycle Finished(outcome)  -> SSE turn_finished  {thread_id, outcome, M, T}
client: SSE reader (opened before the POST) -> gateway-wire mapper -> TransportEvent{turnId:T} -> reducer seals T
```

REPLv3 (M4): the same sequence with `Message{msg_id:T}` → `Ack{T}` → `TurnStarted{T}` → `Status{T, event}` → `Response{T}` → `TurnFinished{T, outcome}`. `Status.event` is the same JSON object the gateway sends as SSE `data:`, so one mapper decodes both.

### 2.3 The three unifying models

- **Event model.** Every adapter emits `TransportEvent` (3.4). The gateway SSE event objects are the canonical progress vocabulary; REPLv3 wraps them in `Status`.
- **Correlation model.** Client-generated ids, echoed by both servers, validated by one function; approval continuations carry the original id (4.1).
- **Capability model.** `features` + `limits` (`WireLimits`) on both servers; the client folds them into `ServerInfo.capabilities` and `ServerInfo.limits`, with fixed legacy defaults when the daemon predates them (3.7).

### 2.4 Engine and scheduling facts that shape behaviour

| Fact | Source | Consequence |
|---|---|---|
| The socket channel is never V2-routed; V2 eligibility is a const allowlist | `ic/src/bridge/router.rs:60`, `:70-102` | No `stream_chunk` on the socket; `Welcome.engine = "v1"` |
| `StreamChunk` is produced only by V2 | `router.rs:4585-4593` | Streaming only via the gateway with `ENGINE_V2=true` |
| `ImageGenerated` is produced only by V1 today | `ic/src/agent/dispatcher.rs:930-962` | M2a ports it to V2 (8.3.6) |
| Non-gateway messages are globally exclusive in the scheduler | `ic/src/agent/dispatch.rs:26-34`, `:169-172`, `:205` | A socket turn serializes with every other channel; documented, not changed |
| Gateway V2 user input runs in up to 4 parallel lanes | `dispatch.rs:7`, `:26-34` | The gateway is the multi-turn path |
| `/quit` falls through V2 routing to the legacy `Quit` arm | `agent_loop.rs:1851`, `:2151`, `:1412` | One choke point for the refusal (8.4) |

---

## 3. Client design (`tui/`)

### 3.1 Stack and OpenTUI version

- Runtime: Bun (engines `bun >= 1.3.14`; host has 1.3.14, CI image `oven/bun:1.4.4`, `.gitlab/ci/defaults.yml:40`). Node is not supported (OpenTUI needs node >= 26.4; the host has 24.20).
- **Pin exactly** `@opentui/core` 0.8.1, `@opentui/react` 0.8.1, `react` 19.3.1, `typescript` 5.9.3, `bun-types` 1.3.14, `@types/react` 19.2.2. Commit `bun.lock`; install with `bun install --frozen-lockfile`.
- Why 0.8.1 rather than vibetui4's 0.6.6:
  - 0.8.0 fixes the Hyprland host-clipboard freeze (#1825) that Ctrl+V on this desktop would hit, native ScrollBox fixes for follow-the-stream transcripts, and frames over 8 MiB no longer freeze the screen (inline images).
  - 0.8.1 adds a native stdout writer thread that keeps key and timer handling responsive when output stalls (protects push-to-talk timing).
  - Every API this client uses has unchanged declarations (`lib/KeyHandler.d.ts`, `renderables/Input.d.ts`, `lib/paste.d.ts`, `audio.d.ts` except one private field). vibetui4 typechecks and passes **31/31 tests, 78 expect() calls** on 0.8.1 (`<research>/design/pkgs/test-081.log`, empty `tsc-081.log`).
  - Breaking changes in 0.7/0.8 (native scene budget, custom renderables, ScrollBox internal parts, ABI) are not used. One layout change matters: zero-height nodes are no longer drawn, so fixed rows use `flexShrink: 0` and explicit heights (3.12).
  - Risks: 0.8.1 is days old; its native library links a decode-only FFmpeg (+3.2 MB). Fallback: 0.6.8 (same API for this client) with `clipboard backend = "wl-paste"` forced, if the supervised smoke test (16) fails.
- Renderer (`tui/src/main.tsx`):

```ts
neutralizeProxyEnv()                  // config/proxy-env.ts: before the first fetch (3.11.4)
const renderer = await createCliRenderer({
  exitOnCtrlC: false,                 // Ctrl+C is handled by controller/keymap.ts (ot081:renderer.d.ts:37)
  useKittyKeyboard: { events: true }, // release events where KKP is negotiated; inert inside tmux (renderer.d.ts:148-154)
  useMouse: true,
  enableMouseMovement: false,         // "drag" tracking: press, drag, release only
  synchronizedOutput: "always",       // 0.8.x default "auto" can show partial frames in tmux (ot081:zig.d.ts:647)
  targetFps: 30,
  clock,                              // SystemClock in production, ManualClock in tests (renderer.d.ts:63)
})
renderer.on("destroy", () => void shutdownControllers())   // parec / hex children never orphan
```

### 3.2 File layout

New top-level `tui/` (as on vibetui4). Files marked "salvage" start from the vibetui4 file named.

```
tui/
  package.json  bun.lock  tsconfig.json  bunfig.toml  .gitignore  README.md
  bin/wingtip                           #!/usr/bin/env bun launcher -> src/main.tsx
  src/
    main.tsx                            thin entry: config, renderer, controllers, mount (salvage v4 main.tsx:21-56 image-protocol flag)
    app.tsx                             layout and composition only
    util.ts                             wrapText, truncate (salvage v4 util.ts)
    sanitize.ts                         terminal-safe text: strip ANSI, C0/C1, bidi controls
    log.ts                              opt-in 0600 metadata-only debug log
    config/  types.ts config.ts flags.ts secret.ts token-file.ts paths.ts tenant-access.ts tenant-select.ts
             state-file.ts instance-lock.ts child-env.ts proxy-env.ts
    state/   model.ts actions.ts ids.ts reducer.ts correlate.ts turn-text.ts layout.ts store.ts coalesce.ts
             approval-measure.ts
    controller/ types.ts supervisor.ts session.ts commands.ts keymap.ts composer-handle.ts credential-input.ts
                approval-reply.ts panel-content.ts footer-hint.ts copy.ts notifier.ts
    transport/  types.ts errors.ts guards.ts event-channel.ts framing.ts sse.ts backoff.ts listener-owner.ts
                gateway.ts gateway-wire.ts control-acks.ts
                socket.ts replv3-wire.ts legacy-wire.ts                      (M4)
    voice/   types.ts errors.ts manager.ts ptt.ts recorder.ts recorder-opentui.ts recorder-parec.ts
             wav.ts levels.ts vad.ts
             sources/ index.ts lunarvoice.ts                                  (M1a)
                      hex-locate.ts hex-http.ts hex-app.ts hex-embedded.ts    (M1b)
    images/  sniff.ts limits.ts attach.ts clipboard.ts paste-paths.ts screenshot.ts protocol.ts data-url.ts
             export.ts
    components/ transcript.tsx turn-view.tsx attachment-bar.tsx header.tsx composer.tsx mic-widget.tsx
                vu-meter.tsx approval-panel.tsx notices.tsx footer.tsx info-panel.tsx tenant-picker.tsx theme.ts
  test/    (section 11.3)
```

Disposition of vibetui4 files:

| vibetui4 file | Disposition |
|---|---|
| `tui/package.json`, `bun.lock` | replaced: caret ranges (`^0.6.6`) become exact pins; scripts `start`, `typecheck` (`tsc --noEmit`), `test` (`bun test`); `"engines":{"bun":">=1.3.14"}`; `"bin"` |
| `tui/tsconfig.json` | kept `jsxImportSource: "@opentui/react"`; add `noUncheckedIndexedAccess`, `noFallthroughCasesInSwitch` |
| `tui/src/app.tsx` (353 lines, the C1/C5/C8 locus) | replaced by composition only; `submittedText()` (`v4:tui/src/app.tsx:394-396`) kept |
| `tui/src/repl-client.ts` | replaced by `transport/socket.ts` + `transport/framing.ts` (M4); `defaultSocketPath()` (`:36-50`) moves verbatim to `config/paths.ts` in M1a |
| `tui/src/protocol.ts` | replaced by `transport/replv3-wire.ts` and `transport/legacy-wire.ts` (M4) |
| `tui/src/turn-buffer.ts` | replaced by `state/turn-text.ts` (same three invariants, per turn, immutable) |
| `tui/src/images.ts` | split: `sniffImageMime` → `images/sniff.ts` without BMP; `attachFile` → `images/attach.ts` (async, bounded); `readClipboard`/`spawnSync` deleted (`images/clipboard.ts` replaces it); `ImageProtocol` → `images/protocol.ts` |
| `tui/src/util.ts` | `wrapText`, `truncate` kept; `decodeDataUrl` moves to `images/data-url.ts` with a MIME allowlist and a size cap |
| `tui/src/components/transcript.tsx` | `ImageBlock`/`ItemBlock` ideas kept; `LiveBlock`/`liveRows` removed (the live turn renders inside the scrollbox, C5) |
| `tui/src/components/attachment-bar.tsx` | kept, plus size/dimension caption and a one-line compact mode |
| tests | `turn-buffer.test.ts` → `test/state/turn-text.test.ts`; `repl-client.test.ts` MockDaemon → `test/mocks/mock-replv3-daemon.ts` (M4); `images.test.ts` → `test/images/{sniff,attach}.test.ts` (BMP now rejected; `mkdtemp` instead of `/tmp/opencode`); `image-output.test.tsx:196` assertion inverted; `input-keys.test.tsx` kept with its stale header fixed; `app-smoke.test.tsx` ported |

### 3.3 State model (`tui/src/state/model.ts`)

A pure reducer over an external store. React reads it with `useSyncExternalStore(store.subscribe, store.getState)`. The reducer never calls `Date.now()`, the network or a child process; every action carries `at` from the injected `Clock` (`ot081:lib/clock.d.ts`), so tests drive time with `ManualClock` (`ot081:testing/manual-clock.d.ts:2`). Secrets (tokens, a credential being typed) and audio never enter the store.

```ts
export type TurnId = string & { readonly __brand: "TurnId" }   // crypto.randomUUID()
export type ApprovalId = string                                 // server request_id (UUID text)
export type ThreadId = string
export type ImageMime = "image/png" | "image/jpeg" | "image/gif" | "image/webp"

export type ConnState =
  | { kind: "connecting"; attempt: number; since: number }
  | { kind: "ready"; since: number }
  | { kind: "reconnecting"; attempt: number; retryAt: number; lastError: string }
  | { kind: "offline"; reason: string; fatal: boolean; since: number }

export type TurnPhase =
  | "pending"            // created; send() in flight
  | "sent"               // send() resolved; no server event yet
  | "queued"             // REPLv3 Ack without TurnStarted, or another turn of this thread is running
  | "running"            // turn.started or the first correlated event
  | "awaiting_approval"  // finished with outcome paused and an approval prompt pending
  | "awaiting_input"     // finished with outcome paused, credential or external gate
  | "background_running" // finished with outcome detached; a late reply will follow
  | "done" | "interrupted" | "failed"
  | "lost"               // connection dropped and the transport cannot resume (legacy socket)
  | "unanswered"         // legacy daemons only: silent for 610 s, or cancelled while silent (rule 12)

export type TurnOrigin = "user" | "control" | "background" | "history"
export type ControlKind = "interrupt" | "steer" | "clear" | "new"   // clear/new: forwarded /clear and /new (3.5 rule 3b)

export interface Activity {
  kind: "thinking" | "tool_started" | "tool_completed" | "tool_result" | "status" | "reasoning" | "skill" | "recovery" | "job"
  text: string; ok?: boolean; at: number          // text sanitized to one line
}
export interface TranscriptImage {
  id: string; mime: ImageMime; bytes: Uint8Array | null; byteLength: number
  width?: number; height?: number; caption?: string   // a server `path` is shown as caption text, never loaded
}
export interface TurnRecord {
  id: TurnId; seq: number; origin: TurnOrigin
  control?: { kind: ControlKind; text: string; ack?: string }
  threadId: ThreadId | null
  input: { text: string; images: TranscriptImage[] } | null
  phase: TurnPhase
  deltas: string                 // streamed text (turn-text.ts), capped at 1 MiB
  finalText: string | null       // canonical text; several finals are joined with a blank line
  activity: Activity[]           // ring buffer, last 50
  images: TranscriptImage[]      // generated images
  approvalIds: ApprovalId[]
  steerNotes: { text: string; ack?: string }[]
  notes: string[]                // one-line annotations: "credential cancelled", "approval expired", 3.7 image note
  error?: { code: string; message: string }
  cost?: { inputTokens: number; outputTokens: number; costUsd: string }
  suggestions?: string[]
  serverMessageId?: string       // gateway 202 message_id (informational)
  recovered?: boolean            // filled from /api/chat/history after a gap
  finish?: { outcome: FinishOutcome; at: number; droppedStatus?: number }
  credential?: { extensionName: string }   // this turn carried a credential; its text is never stored
  createdAt: number; sentAt?: number; firstEventAt?: number; lastEventAt?: number; endedAt?: number
}
export type ApprovalDecision = "approve" | "always" | "deny"
export interface ApprovalPrompt {
  id: ApprovalId; turnId: TurnId | null; threadId: ThreadId | null
  toolName: string; description: string; parameters: string; allowAlways: boolean
  truncated: boolean               // REPLv3 parameters_truncated: may only be denied (3.6.2)
  source: "live" | "history" | "pending_gates" | "legacy_text"
  state: "pending" | "answering" | "resolved" | "expired" | "failed"
  decision?: ApprovalDecision; createdAt: number
  focusedAt: number | null         // when it last became the focused prompt (1 s rule, 3.6.2)
  viewedFull: boolean              // the full view (Alt+V) was opened for this request (3.6.2)
}
export interface Notice {
  id: string; level: "info" | "warn" | "error"
  source: "client" | "transport" | "server" | "voice" | "image" | "job" | "auth"
  text: string; sticky: boolean; expiresAt: number | null; at: number   // info 6 s, warn 12 s, errors sticky
}
export interface PendingImage {
  id: string; mime: ImageMime; bytes: Uint8Array; filename: string | null
  width: number; height: number; source: "clipboard" | "file" | "drop" | "shot"
}
export interface ComposerState {
  pendingImages: readonly PendingImage[]
  startedAt: number | null         // first edit or insert since the composer was last empty (3.10.3)
  lines: number; bytes: number     // visual lines and UTF-8 bytes, reported by onContentChange (3.10.5)
  history: readonly string[]       // in memory only, newest last, at most 50 entries (3.10.5)
  guard: { text: string; reason: "approval_keyword" | "restart"; until: number } | null
  confirm: { approvalId: ApprovalId; decision: "approve" | "deny"; at: number } | null   // 3.10.3
  restore: { text: string; images: readonly PendingImage[] } | null
  credential: { extensionName: string; threadId: ThreadId | null; turnId: TurnId | null; startedAt: number
                chars: number      // length of the masked buffer in controller/credential-input.ts
                escArmedAt: number | null
                requestId?: string; instructions?: string; authUrl?: string; setupUrl?: string } | null
}
export type PanelKind = "help" | "voice" | "privacy" | "devices" | "transport" | "threads" | "approval"
export interface PanelState { kind: PanelKind; scroll: number; approvalId?: ApprovalId }
export interface UiState { tmux: boolean; kkpRelease: boolean; blurred: boolean; color: boolean; reduceMotion: boolean }
export interface AppState {
  profile: string; tenant: string | null; conn: ConnState; server: ServerInfo | null; threadId: ThreadId | null
  turns: Readonly<Record<TurnId, TurnRecord>>; order: readonly TurnId[]       // newest 500
  inflight: readonly TurnId[]; controls: readonly TurnId[]                      // unsealed, send order
  strayInterruptAck: { until: number } | null                                   // legacy only
  approvals: Readonly<Record<ApprovalId, ApprovalPrompt>>; approvalQueue: readonly ApprovalId[]
  composer: ComposerState; voice: VoiceUiState; notices: readonly Notice[]
  panel: PanelState | null; ui: UiState
  otherThreadsUnread: number; view: { width: number; height: number }; seq: number
}
/** Implemented by state/store.ts; declared here so controllers and voice compile before it exists (F6). */
export interface Store {
  getState(): AppState
  dispatch(action: Action): void
  dispatchMany(actions: readonly Action[]): void
  subscribe(listener: () => void): () => void
}
```

`VoiceUiState`, `PttPhase` and `PttMode` are declared in `voice/types.ts` (5.2), `ComposerHandle` in `controller/types.ts` (3.10.5), `StateFile` and `TokenSource` in `config/types.ts` (3.11). All of them belong to the scaffold task, so no implementation module imports another task's implementation for its types (F6).

Actions (`state/actions.ts`): `conn/connecting|ready|lost|offline`, `thread/selected`, `turn/created|sent|send_failed|note`, `control/created|send_failed`, `evt` (one `TransportEvent`), `resync/applied`, `approval/answering|answered|focus_next|focused|viewed_full|confirm_requested|confirm_cleared`, `composer/attach|remove_last|clear_attachments|metrics|started|cleared|history_push|guard|guard_clear|restore_consumed|credential_chars|credential_esc_armed|credential_done`, `voice/state|level`, `panel/open|scroll|close`, `ui/focus|blur`, `notice/push|dismiss|tick`, `view/resize`. Field lists as in `design/area-client.md` 2.3 plus the fields above; every action has `at: number`.

Store (`state/store.ts`): `createStore(initial, reduce): Store`, plus `useAppState()`. Transport events pass through `state/coalesce.ts`, which flushes at most every 33 ms and merges consecutive `turn.delta` events of one turn; user intents dispatch immediately.

### 3.4 Common event model (`tui/src/transport/types.ts`)

Every adapter maps its wire format onto one union. `turnId` is `null` when the server echoed no client id.

```ts
export type FinishOutcome =
  | "replied" | "no_reply" | "paused" | "interrupted" | "detached"
  | "failed" | "timed_out" | "cancelled" | "unknown"          // identical strings on both transports (4.1)
export type WireApproval = Omit<ApprovalPrompt, "state" | "createdAt" | "turnId" | "focusedAt" | "viewedFull">
/** gateway gate_resolved strings and REPLv3 ApprovalResolved, normalized (3.5 rule 3b) */
export type ApprovalResolution = "approve" | "always" | "deny" | "credential" | "external"
  | "cancelled" | "terminated" | "expired" | "unknown"

export type TransportEvent =
  | { kind: "turn.accepted"; turnId: TurnId }                                // REPLv3 Ack
  | { kind: "turn.started"; turnId: TurnId | null; threadId?: ThreadId }     // gateway turn_started, REPLv3 TurnStarted
  | { kind: "turn.delta"; turnId: TurnId | null; threadId?: ThreadId; text: string }
  | { kind: "turn.activity"; turnId: TurnId | null; threadId?: ThreadId; activity: Omit<Activity, "at"> }
  | { kind: "turn.image"; turnId: TurnId | null; threadId?: ThreadId; image: { mime: ImageMime; bytes: Uint8Array; caption?: string } }
  | { kind: "turn.approval"; turnId: TurnId | null; threadId?: ThreadId; approval: WireApproval }
  | { kind: "turn.approval_resolved"; approvalId: ApprovalId; resolution: ApprovalResolution }
  | { kind: "turn.auth_required"; turnId: TurnId | null; threadId?: ThreadId; extensionName: string;
      requestId?: string; instructions?: string; authUrl?: string; setupUrl?: string }
  | { kind: "turn.final"; turnId: TurnId | null; threadId?: ThreadId; text: string }
  | { kind: "turn.finished"; turnId: TurnId; outcome: FinishOutcome; droppedStatus?: number }
  | { kind: "turn.settle_hint"; threadId?: ThreadId; signal: "done" | "rejected" | "awaiting_approval" | "thread_terminal" }  // legacy gateway only
  | { kind: "turn.error"; turnId: TurnId | null; threadId?: ThreadId; code: string; message: string; retryable: boolean }
  | { kind: "turn.cost"; turnId: TurnId | null; threadId?: ThreadId; inputTokens: number; outputTokens: number; costUsd: string }
  | { kind: "turn.suggestions"; turnId: TurnId | null; threadId?: ThreadId; suggestions: string[] }
  | { kind: "notice"; level: "info" | "warn" | "error"; source: Notice["source"]; text: string }
  | { kind: "background"; threadId?: ThreadId; text: string }               // proactive or job message, no turn
  | { kind: "other_thread"; threadId: ThreadId }
  | { kind: "resync.required"; reason: "gap" | "reconnect" | "overflow" }
```

**Gateway SSE → TransportEvent** (`transport/gateway-wire.ts`; every event may carry `message_id` and `client_turn_id`; `turnId = client_turn_id ?? null`). The same function decodes REPLv3 `Status.event`.

| SSE `type` | TransportEvent |
|---|---|
| `turn_started {thread_id}` (NEW) | `turn.started` (ignored when `client_turn_id` is absent or unknown) |
| `turn_finished {thread_id, outcome}` (NEW) | `turn.finished` (ignored when `client_turn_id` is absent or unknown) |
| `response {content, thread_id}` | `turn.final`; content starting with `Error: ` becomes `turn.error{code:"agent_error"}` (V1 error replies, `agent_loop.rs:2192`); without correlation on a correlated gateway: `background` |
| `stream_chunk {content}` | `turn.delta` |
| `thinking {message}` | `turn.activity{kind:"thinking"}` |
| `tool_started {name}` / `tool_completed {name, success, error?, parameters?}` / `tool_result {name, preview}` | `turn.activity` (`tool_started` / `tool_completed` with `ok` / `tool_result`, preview truncated to 200 chars) |
| `status {message}` | empty `message` ignored (unmapped common events serialize as empty status); else `turn.activity{kind:"status"}`; on legacy gateways also `turn.settle_hint` for `Done`, `Rejected`, `Awaiting approval` |
| `approval_needed {...}`, `gate_required {resume_kind:{Approval:{allow_always}}}` | `turn.approval` (deduplicated by `request_id`) |
| `gate_required` Authentication | `turn.auth_required` with `requestId`; External: `notice{source:"auth"}` |
| `gate_resolved {request_id, resolution}` | `turn.approval_resolved`; `approved` → `approve`, `approved_always` → `always`, `denied` → `deny`, `credential_provided` → `credential`, `external_callback` → `external`, `cancelled`, `thread_terminated` → `terminated`, `expired`, anything else → `unknown` (`ic/src/bridge/router.rs:1236`, `:1346`, `:2428`, `:2945-2950`, `:2980`, `:3033`; `ic/src/bridge/router/auth.rs:115`) |
| `auth_required {extension_name, instructions?, auth_url?, setup_url?}` | `turn.auth_required` |
| `auth_completed` | `notice{source:"auth"}` |
| `image_generated {data_url, path?}` | `turn.image` after `images/data-url.ts` validation |
| `suggestions`, `turn_cost`, `reasoning_update`, `skill_activated` | `turn.suggestions`, `turn.cost`, `turn.activity{reasoning}`, `turn.activity{skill}` |
| `turn_recovery {recovery_state}` | `failed` → `turn.error{code:"recovery_failed"}`, else `turn.activity{recovery}` |
| `job_started`, `job_result`, `job_status` | `notice{source:"job"}` (URLs shown as text, never opened) |
| `stream_gap {missed_events}` | `resync.required{gap}` |
| `thread_state_changed {to_state}` | `Completed`/`Done`/`Failed` → `turn.settle_hint{thread_terminal}` (used only without lifecycle events) |
| `error {message}` | `turn.error{code:"server"}` (defined, not produced today) |
| `heartbeat`, keepalive comment | watchdog only |
| `job_message`, `job_tool_use`, `job_tool_result`, `child_thread_spawned`, `extension_status`, unknown types | ignored |

**REPLv3 frames → TransportEvent** (`transport/replv3-wire.ts`, M4; `turnId = msg_id`):

| Frame | TransportEvent |
|---|---|
| `Ack{msg_id}` | `turn.accepted` |
| `TurnStarted{msg_id}` | `turn.started` |
| `Status{msg_id?, event}` | `event` through the gateway mapper above, with `turnId = msg_id` |
| `ApprovalRequest{...}` / `ApprovalResolved{...}` / `AuthRequired{...}` | `turn.approval` (`truncated = parameters_truncated`) / `turn.approval_resolved` (`approve`, `always`, `deny`, `cancelled` map one to one) / `turn.auth_required` |
| `Response{content, msg_id?}` | `turn.final`; no `msg_id`: `background` |
| `TurnFinished{msg_id, outcome, dropped_status}` | `turn.finished` (same outcome strings) |
| `Error{msg_id?, req_id?, code, message, retryable, fatal}` | `msg_id`: terminal `turn.error`; `req_id`: rejects the pending `transcribe()`; neither: `notice`; `fatal`: the supervisor reconnects |
| `TranscribeResult{req_id, ...}` | resolves `transcribe()`; no store event |
| `Notice{kind, level, text}` | `background` → `background`; `broadcast` and `shutdown` → `notice` |
| `"Ping"` / `"Pong"` | answered with `"Pong"` / keepalive watchdog |

**Legacy REPL v1** (`transport/legacy-wire.ts`, M4): `Response{content}` → `turn.final{turnId:null}`; `Ping`/`Pong` keepalive; anything else ignored.

### 3.5 Correlation and sealing (`tui/src/state/correlate.ts`)

Applied to every `evt`, in order:

1. **Thread filter** (gateway). An event whose `threadId` differs from `state.threadId` becomes `other_thread` (bumps `otherThreadsUnread`); approvals in other threads become a warn notice.
2. **Exact match.** `turnId` names a known record (user or control): apply it. Records exist before the request is written (`turn/created` precedes `send()`), so events that overtake the gateway's 202 still attach.
3. **Lifecycle sealing** (`capabilities.lifecycle`). `turn.finished` decides the phase:

   | outcome | phase | sealed |
   |---|---|---|
   | `replied` | `done` | yes |
   | `no_reply` | `done` ("no reply"); if an approval prompt for this turn is still pending: `awaiting_approval` (defensive) | yes, unless awaiting |
   | `paused` | `awaiting_approval` if an approval prompt for this turn is pending, else `awaiting_input` | no |
   | `interrupted` | `interrupted` (deltas kept) | yes |
   | `detached` | `background_running` ("still running; the result will arrive here") | no |
   | `failed`, `timed_out` | `failed` (message from a preceding `turn.error` or final) | yes |
   | `cancelled` | `interrupted` ("cancelled before it started") | yes |
   | `unknown` | `done` | yes |

   A later `turn.started` or any correlated progress event moves an unsealed `awaiting_*` or `background_running` turn back to `running`. On a sealed record: `turn.delta` and progress are dropped; a correlated `turn.final` is appended as late text (marked "late"); a second `turn.finished` is ignored. `droppedStatus > 0` adds "N progress events were dropped".

   3b. **Ending paused turns** (F2). A paused turn whose gate is discarded never gets another lifecycle event, so the reducer seals an `awaiting_approval` or `awaiting_input` turn as `interrupted` (deltas kept, its prompts dropped, a note naming the cause) when:
   - `turn.approval_resolved` for one of its prompts has resolution `cancelled`, `terminated` or `expired`; `approve`, `always`, `deny`, `credential` and `external` resume the turn instead;
   - a control record of kind `interrupt`, `clear` or `new` for the same thread is sealed with any reply other than `Nothing to interrupt.` (V2 discards the scope's gates without a `gate_resolved`, `ic/src/bridge/router.rs:2526-2543`, `:3268-3285`; V1 only flips the thread state, `ic/src/agent/thread_ops.rs:1133-1149`); on legacy daemons the matched control ack of rule 5a has the same effect;
   - its credential flow ends locally: `auth-cancel` returned 2xx, or credential mode ended without sending (3.6.4).
   Turns in `awaiting_*` or `background_running` never count against the local in-flight cap (3.6.1).
4. **No lifecycle, correlated** (none in this design; kept for robustness): a correlated `turn.final` seals.
5. **Uncorrelated `turn.final`** (legacy gateway and legacy socket only):
   - a. If `controls` is non-empty and `isControlAck(text)` (`transport/control-acks.ts`): seal the oldest control with `ack = text`. If the ack is `Interrupted.`, also mark the oldest *running* user turn `interrupted` (a silent, never-started turn does not absorb it; D1) and set `strayInterruptAck = {until: at + 5000}` unless the engine is V2 (V1 also answers the aborted turn with `Interrupted.`, `agent_loop.rs:2193`; V2 answers once, `router.rs:3270-3285`).
   - b. Else if `strayInterruptAck` is active and the text is `Interrupted.`: swallow once.
   - c. Else if `inflight` is non-empty: seal the oldest inflight user turn as `done`.
   - d. Else: append a `background` record.
6. **Correlated transport, final without id** (gateway with capabilities, REPLv3 `Notice{background}`): never matched FIFO; a `background` record.
7. **Uncorrelated progress** (legacy only): attaches to the oldest inflight turn; with none, deltas and images open a `background` record and activity is dropped.
8. **Uncorrelated approval** (legacy gateway): the prompt takes the oldest inflight turn's id (or `null`).
9. **`turn.error`**: with an id, terminal for that record (`failed`); without one, seals the oldest inflight turn on legacy paths, else an error notice.
10. **`turn.auth_required`**: the composer enters credential mode (3.6.4).
11. **Settle hints** (legacy gateway only): `status` Done/Rejected or `thread_state_changed` to `Completed`/`Done`/`Failed` start a 10 s grace; if the oldest inflight turn is still open the session controller calls `Transport.resync()`.
12. **No reply yet, then unanswered** (legacy only; D1, 2026-11-03): after 15 s without any event the oldest silent turn gets the note "no reply yet" and keeps its FIFO place (a busy daemon may still answer); its first event clears the note. It is sealed `unanswered` only after 610 s (`YOUNG_FAILED_TURN_MS`) or when the user cancels it (Ctrl+S or `/interrupt`): "Nothing to interrupt." seals the silent turns created before the interrupt as `unanswered`; "Interrupted." with only silent turns seals them `interrupted`. Trade-off: a message the daemon swallowed (an approval keyword) keeps its slot and takes the next reply unless cancelled.
13. **Daemon restart** (D2): `GET /api/gateway/status` `uptime_secs` gives the daemon start; a later connect whose start is newer (2 s slack plus 1 ms per second of drift) seals every open record created before it as `lost (daemon restarted)` after history matching, except a turn paused at a gate the snapshot still lists. Probe failure means no detection.

Known control acks (`transport/control-acks.ts`): `Interrupted.` (`ic/src/bridge/router.rs:55`, `ic/src/agent/thread_ops.rs:1146`), `Nothing to interrupt.` (`router.rs:57`, `thread_ops.rs:1148`), `Steer note queued` (`thread_ops.rs:1171-1174`), `Steer note queue full` (`thread_ops.rs:1185`), `Thread completed. Start a new thread` (`thread_ops.rs:1178`), `Thread cleared.` (`thread_ops.rs:1258`), the prefix `New thread: ` (`thread_ops.rs:2129`), `Conversation cleared.` (`router.rs:3397`), `Started new conversation.` (`router.rs:3388`). The session controller records forwarded `/clear` and `/new` (exact, case-insensitive) as control records of kind `clear` and `new`.

### 3.6 Turn controls, approvals, credentials, notices, connection

#### 3.6.1 Queue, interrupt, steer

| Intent | Gateway | REPLv3 | Legacy socket |
|---|---|---|---|
| Enter while idle or busy | `POST /api/chat/send` with `client_turn_id`; shown `queued #n` while another turn of the thread runs (same-thread messages run FIFO) | `Message{msg_id}`; `Ack` without `TurnStarted` = queued | `Message{content}` |
| Local cap | at most `min(8, limits.max_in_flight ?? 8)` user turns in `pending`, `sent`, `queued` or `running` (paused and detached turns do not count, 3.5 rule 3b); the next Enter is refused with a notice | | |
| Ctrl+S (Ctrl+Enter when KKP is active) | with text or images: interrupt, then send, awaited in that order; with an empty composer and no pending images: interrupt only; in credential mode: submits the credential (3.6.4) | `Interrupt` then `Message` (or `Interrupt` only) | text `/interrupt` then the message |
| `/interrupt`, `/stop` | control record; `POST /api/chat/send {"content":"/interrupt","client_turn_id":<new>}` (priority control, `ic/src/agent/dispatch.rs:13-18`); reply correlated to the control; the stopped turn finishes `interrupted` | `Interrupt{msg_id}` → `Ack`, `TurnStarted`, `Response("Interrupted.")` or `Response("Nothing to interrupt.")`, `TurnFinished`; queued prompts it removed finish `cancelled` | text; ack matched by 3.5 rule 5 |
| `/steer <note>` | `/steer <note>` as content with its own id | `Steer{msg_id, content}` → `Response("Steer note queued ...")`, `TurnFinished` | text |

Nothing is ever re-sent automatically (a resend could repeat a tool action). While not `ready`, Enter keeps the text and shows "not connected".

#### 3.6.2 Approvals

- Sources: gateway `approval_needed`, `gate_required` (Approval), history `pending_approval`, `GET /api/chat/pending-gates`; REPLv3 `ApprovalRequest` (replayed after reconnect). Deduplicated by `request_id`.
- Panel (`components/approval-panel.tsx`): tool name, description (up to 6 wrapped lines), parameters (up to 8 wrapped lines; redacted server-side, then sanitized), `[Alt+Y] approve`, `[Alt+A] always` (only when `allowAlways`), `[Alt+N] deny`, `[Alt+V] view all`, mouse-clickable, a `1/3` counter, and the hint `say or type yes / no, then Enter twice`.
- Hidden content (SEC-04, UX-12). The pure `state/approval-measure.ts` exports `measureApproval(prompt: WireApproval, width: number): { description: string[]; parameters: string[]; hiddenLines: number; hiddenBytes: number }`, built on `util.ts` `wrapText`; the panel and the controller use the same result. When `hiddenLines > 0` the panel shows `[+N lines hidden - Alt+V view all]` and every approve action (Alt+Y, Alt+A, `/approve`, `/always`, a confirmed typed or dictated reply) is refused with "open the full parameters first (Alt+V)" until the full view has been opened for that `request_id` (`ApprovalPrompt.viewedFull`). Deny is always allowed. The client never truncates parameters; the gateway sends them whole (`ic/src/channels/web/mod.rs:504-516`, V2 redacts but does not truncate, `ic/src/bridge/router.rs:240-242`).
- Full view: Alt+V or `/approval view` opens the info panel (3.12.3) with kind `approval`: tool, full description and full parameters in a scrollbox. Alt+Y, Alt+A and Alt+N work inside it; opening it sets `viewedFull`.
- Invisible characters: in the panel and the full view only, U+00AD, U+180E, U+200B-U+200F, U+2060-U+2064, U+FEFF and U+E0000-U+E007F render as visible `\u{XXXX}` escapes in a distinct style, so text hidden inside a visible line shows. Elsewhere `sanitize.ts` leaves them alone.
- Truncated prompts: a REPLv3 `ApprovalRequest` with `parameters_truncated: true` (4.3.2) can only be denied; the panel says "parameters too large to review here (N bytes); review in the web UI".
- Freshness (SEC-08): Alt+Y, Alt+A, Alt+N, `/approve`, `/always` and `/deny` act only on the focused prompt and only after it has been focused for at least 1 s (`focusedAt`); the slash commands additionally require that the prompt existed before the first keystroke of the current composer text (`createdAt < composer.startedAt`). Otherwise the input is consumed with the notice "approval just arrived; press again". Alt+] moves the focus and restarts the 1 s window.
- Answer: `approval/answering` → `Transport.approve({approvalId, decision, threadId, turnId})` → `approval/answered`.
  - Gateway: `POST /api/chat/approval {"request_id","action","thread_id","client_turn_id":<owning TurnId>}`; 202 completes; 404 marks `expired`.
  - REPLv3: `Approval{msg_id:<new id>, request_id, decision}`; its `Ack` completes; `Error{unknown_request|no_pending_approval}` marks `expired`. The approval's own id is not a `TurnRecord`.
  - `gate_resolved` / `ApprovalResolved` removes the prompt and may seal its turn (3.5 rule 3b).
- Typed and dictated replies: 3.10.3. Typed text never yields `always`.
- Desktop notification (UX-12): when the terminal is blurred (`useBlur` / `useFocus`, `otr081:src/hooks/use-blur.d.ts`, `use-focus.d.ts`) and a new prompt arrives, `controller/notifier.ts` spawns `["notify-send", "-a", "wingtip", "--", "LunarWing <tenant>", "approval needed: <tool>"]` with the child environment of 3.11 (argv only; the tool name is sanitized and cut to 64 characters; the description and parameters are never included), at most once per 10 s. `[ui] notify = "approvals" | "all" | "off"`, default `approvals`; `all` also notifies when a turn finishes while blurred. A missing `notify-send` disables it silently. Terminal bells are not used (tmux on this host has `bell-action none`).
- Legacy socket (no prompts arrive): a per-turn stall notice after 120 s without frames; `/approve`, `/always`, `/deny` send `yes`, `always`, `no` as text.

#### 3.6.3 Notices and errors

Turn-scoped errors render inside the turn, never as assistant text (D11 on the client side). Everything else is a `Notice`: at most two visible, info expires after 6 s, warn after 12 s, errors sticky until Esc or a newer error, list capped at 20. Output longer than one line (help, `/voice`, `/transport`, `/thread list`, full approval text) goes to the info panel (3.12.3), never to notices.

#### 3.6.4 Credential mode (`auth_required`)

The daemon consumes the next message on the thread as the credential on V1 (`agent_loop.rs:1927-1975`) and V2 (`router.rs:3784-3799`); the gateway also offers the dedicated `auth-token` route. Credential mode makes this explicit and keeps the secret out of chat (SEC-03, UX-13):
- `turn.auth_required` sets `composer.credential` (extension, owning turn, `requestId` when a `gate_required{Authentication}` supplied it, instructions and URLs shown as sanitized text and never opened) and blurs the textarea.
- Input capture: the global `useKeyboard` and `usePaste` handlers call `preventDefault()` on every key and paste and feed `controller/credential-input.ts`, a masked buffer held outside the store (`push(text)`, `backspace()`, `clear()`, `take(): Secret`, `get length()`). The store sees only `credential.chars`. The composer box shows `credential for <extension>: **** (N chars)`; the secret is never rendered, logged, put in history or written to the debug log. Dictation, image gestures, slash commands, `//`, `/steer`, `/interrupt` and approval keywords are disabled.
- Submit: Enter, Ctrl+S and Ctrl+Enter all call `submitCredential()`; none of them interrupts or sends chat.
  - Gateway: `POST /api/chat/auth-token {"extension_name","token","client_turn_id":<owning TurnId>}` plus `request_id` and `thread_id` together when known (`ic/src/channels/web/types.rs:1423-1429`; `client_turn_id` is NEW, 8.3.1). On V2 the resume message carries the owning turn's id, so `turn_started` and `turn_finished` continue that turn (F1).
  - REPLv3 and legacy socket: `Message{content:<secret>, msg_id:<new id>}`; the daemon's auth intercept consumes it.
- Cancel: the first Esc arms (`credential.escArmedAt`, footer "press Esc again to cancel the credential"); a second Esc within 2 s cancels.
  - Gateway: `POST /api/chat/auth-cancel {"extension_name","client_turn_id"}` (plus the ids when known). On 2xx the owning turn is sealed `interrupted` with the note "credential cancelled" (3.5 rule 3b). V2 also broadcasts `gate_resolved{resolution:"cancelled"}` (`ic/src/bridge/router/auth.rs:102-121`).
  - Socket: `Interrupt{msg_id:<new id>}` (legacy socket: the text `/interrupt`), which makes the daemon leave auth mode (any control submission clears it, `agent_loop.rs:1966-1971`); then the owning turn is sealed as above.
- Leaving the mode without sending (the buffer is zero-filled; warn "credential mode ended; nothing was sent"): on the socket after a reconnect, a thread rotation, any `Interrupt`, `Steer` or `Approval` the client sends, or `ApprovalResolved{cancelled}`; when the engine is V1 or unknown (socket, legacy gateways, or a gateway reporting `engine:"v1"`), 290 s after `auth_required`, below the daemon's 300 s TTL (`ic/src/agent/session.rs:141`); on V2 gateways when `gate_resolved` for `requestId` or `auth_completed` for the extension arrives.
- The turn stores only `credential:{extensionName}`; the transcript shows "[credential sent to <extension>]".
- Daemon-side logging: the gateway route never logs the token. On the socket path the daemon's `SubmissionParser` logs every message's full text at debug level (`ic/src/agent/submission.rs:17`), which includes a credential; M4 changes that line to log only the length (8.6.1).

#### 3.6.5 Connection states

```
connecting(attempt) --ok--> ready --loss--> reconnecting(attempt, retryAt) --timer--> connecting
     |                         +--/transport reconnect--> connecting                    |
     +--fatal (second 401, 403, EACCES, foreign socket or listener owner)--> offline(fatal)
/quit from any state --> offline("quit") --> exit
```

`controller/supervisor.ts`: `info = await transport.connect(signal)` → `conn/ready` → `for await (ev of transport.events()) coalescer.push(ev)` → on end `conn/lost` → `backoff.next()` → repeat. Backoff (`transport/backoff.ts`): 500 ms base, factor 2, cap 15 s, ±20 % jitter, reset after 30 s ready. On loss, gateway turns stay open (resync from history); REPLv3 turns stay open (thread resume); legacy socket turns become `lost`.

Listener ownership (SEC-01): before the first token-bearing request of every connect and reconnect, and before every later token-bearing request, `GatewayTransport` runs `assertListenerOwner` (3.8.1) when the endpoint is a loopback or tenant URL. No matching listener yields `unreachable` (retry, nothing sent); a listener owned by another uid yields `forbidden` (fatal, `offline`, "port 10000 is held by uid 1008, expected 1010 (derpy)"). The Hex sources run the same check against the user's own uid (5.3.2).

### 3.7 Transport interface and capability model (`tui/src/transport/types.ts`)

```ts
export type TransportKind = "gateway" | "socket"
export type ProtocolFlavor = "gateway-chat" | "gateway-legacy" | "replv3" | "repl-legacy"
export type Tri = boolean | "unknown"

export interface Limits {                     // folded from WireLimits (4.1) or legacy defaults
  maxContentBytes: number; maxImages: number; maxImageBytes: number; maxTotalImageBytes: number
  maxRequestBytes: number                     // gateway JSON body, or socket frame incl. "\n"
  imageMimeTypes: readonly ImageMime[]
  maxTranscribeWavBytes: number               // decoded WAV bytes; 0 = no server transcription
  transcribePerMinute: number | null; chatPerMinute: number | null; maxInFlight: number
}
export interface Capabilities {
  streaming: Tri; activity: boolean; images: boolean; generatedImages: Tri; approvals: boolean
  transcribe: boolean; correlation: boolean; lifecycle: boolean
  interrupt: "native" | "text"; steer: "native" | "text"
  threads: boolean; history: boolean; backgroundMessages: boolean; resume: boolean
}
export interface ServerInfo {
  transport: TransportKind; protocol: ProtocolFlavor; protocolVersion: number | null
  serverVersion: string | null; agentName: string | null; engine: "v1" | "v2" | "unknown"
  sessionId: string | null; threadId: ThreadId | null
  endpointLabel: string                       // socket path or URL origin; never a token
  credential: "master" | "client" | "none" | "unknown"
  capabilities: Capabilities; limits: Limits; warnings: readonly string[]
}
export interface OutgoingImage { mime: ImageMime; bytes: Uint8Array; filename: string | null }
export interface SendRequest { turnId: TurnId; text: string; images: readonly OutgoingImage[] }
export interface ControlRequest { requestId: TurnId; text?: string }
export interface ApproveRequest { approvalId: ApprovalId; decision: ApprovalDecision; threadId: ThreadId | null; turnId: TurnId | null }
export interface TranscribeRequest { requestId: string; wav: Uint8Array; signal: AbortSignal }
export interface TranscribeResult { text: string; language?: string; model?: string; elapsedMs?: number }
export interface ResyncSnapshot { threadId: ThreadId; turns: readonly HistoryTurn[]; approvals: readonly WireApproval[] }
export interface HistoryTurn { userInput: string; response: string | null; state: string; startedAt: string; completedAt: string | null }
export type ThreadSpec = { kind: "last" } | { kind: "new" } | { kind: "assistant" } | { kind: "id"; id: ThreadId }

export interface Transport {
  readonly kind: TransportKind
  connect(signal: AbortSignal): Promise<ServerInfo>        // rejects with TransportError
  events(): AsyncIterable<TransportEvent>                    // single consumer; throws TransportError on loss
  send(req: SendRequest): Promise<{ serverMessageId?: string }>
  interrupt(req: ControlRequest): Promise<void>
  steer(req: ControlRequest & { text: string }): Promise<void>
  approve(req: ApproveRequest): Promise<void>
  authToken?(req: { extensionName: string; token: Secret; requestId?: string; threadId?: ThreadId }): Promise<void>  // gateway
  authCancel?(req: { extensionName: string; requestId?: string; threadId?: ThreadId }): Promise<void>               // gateway
  transcribe?(req: TranscribeRequest): Promise<TranscribeResult>  // check capabilities.transcribe first
  resync?(): Promise<ResyncSnapshot>                              // gateway
  selectThread?(spec: ThreadSpec): Promise<ThreadId>              // gateway threads; REPLv3 rotates the thread token
  close(reason?: string): Promise<void>
}
```

`transport/errors.ts`: `TransportError(code, message, {status?, retryAfterMs?, fatal?})` with codes `unreachable | auth | forbidden | permission_denied | not_found | bad_request | too_large | rate_limited | busy | unavailable | timeout | protocol | closed | unsupported | server`; `auth`, `forbidden`, `permission_denied` are fatal by default. Messages are built from status codes and server error codes only and never contain a token or a request body. `transport/event-channel.ts`: bounded `AsyncIterable` (10 000 items); on overflow it drops queued `turn.delta` items and pushes one `resync.required{overflow}`.

**Folding server capabilities into `ServerInfo`:**

| ServerInfo field | Gateway with capabilities (4.2.2) | Legacy gateway (404) | REPLv3 Welcome (4.3.3) | Legacy socket |
|---|---|---|---|---|
| `engine` | `engine` | `"unknown"` | `engine` | `"v1"` |
| `streaming` | `stream` in features | `"unknown"` until the first `stream_chunk` | `stream` in features | `false` |
| `activity` | `status` in features | `true` | `status` in features | `false` |
| `images` | `attachments` in features | `true` (client validates) | `attachments` in features | `false` |
| `generatedImages` | `true` | `"unknown"` (no V2 image arm before M2a) | `true` | `false` |
| `transcribe` | `transcribe` in features | `/api/voice/status.configured` | `transcribe` in features | `false` |
| `approvals`, `threads`, `backgroundMessages`, `resume` | `true` | `true` | `true` | `false` |
| `correlation`, `lifecycle` | `true` | `false` | `true` | `false` |
| `history` | `true` | `true` | `false` | `false` |
| `interrupt` / `steer` | `"text"` / `"text"` | `"text"` / `"text"` | `"native"` / `"native"` | `"text"` / `"text"` |
| `limits` | `WireLimits` | 100000 B, 5 × 5 MiB, 7 MiB total, 10 MiB body, 4 types, `maxTranscribeWavBytes = floor(max_audio_b64_len*3/4)`, 30/min, 10/min | `WireLimits` (`maxRequestBytes = max_frame_bytes`, `maxInFlight = max_in_flight`) | 100000 B, no images, no transcription |

`maxTranscribeWavBytes = floor(max_transcribe_b64_bytes * 3 / 4)` (18 MiB on the gateway, 12 MiB on REPLv3). `imageMimeTypes` is intersected with the client allowlist.

**Degradation** (`components/*` read only `ServerInfo`):

| Capability | When false or unknown |
|---|---|
| `streaming` | activity lines and an elapsed timer ("waiting for reply, 12 s"); the final appears at once; no caret |
| `activity` | spinner and timer; stall notice after 120 s (legacy) |
| `images` | image gestures refused with "this daemon does not accept images (legacy REPL)"; pending images kept, Enter with images blocked |
| `generatedImages` | `/transport` shows "generated images: not delivered on this engine"; a turn whose `image_generate` or `image_edit` tool completes with `ok` and no `turn.image` before it seals gets the note "image generated, but this daemon does not deliver images to clients (needs the M2a daemon)" (UX-03) |
| `approvals` | no panel; stall notice; `/approve`, `/always`, `/deny` send text keywords |
| `transcribe` | `lunarvoice` skipped in the fallback order; when no source is available the mic widget shows `(no mic)` and a PTT press shows the reason per source (5.4) |
| `correlation` / `lifecycle` | FIFO correlation and settle hints (3.5 rules 5-12); "legacy daemon" badge |
| `history`, `resume` | in-flight turns become `lost` on reconnect |
| tmux (`capabilities.multiplexer === "tmux"`, `ot081:types.d.ts:87`) | images render as blocks |
| no KKP release seen | toggle and repeat-inferred hold (section 7) |
| no recorder (no PulseAudio/ALSA, e.g. running as a tenant uid) | voice unavailable ("no recorder: <reason>"); Hex-recorded capture sources are deferred (13) |
| listener owner not verifiable (a non-tenant, non-loopback URL) | sticky warning "listener owner not verified (remote URL)"; `https` recommended |

### 3.8 GatewayTransport (`tui/src/transport/gateway.ts`)

```ts
export interface GatewayTransportOptions {
  baseUrl: URL; token: TokenSource; thread: ThreadSpec; clock: Clock; timezone: string
  stateFile: StateFile; fetchImpl?: typeof fetch
  expectedListener: { uid: number; label: string } | null   // 3.8.1; null only for non-loopback, non-tenant URLs
  readProc?: ProcReader                                     // injected in tests (transport/listener-owner.ts)
}
// TokenSource and StateFile live in config/types.ts (T1.1); config/tenant-access.ts and config/token-file.ts implement TokenSource.
export interface TokenSource { current(): Secret; reload(): Promise<boolean>; describe(): string
  kind(): "client" | "master" | "file" }                                                         // describe() never shows the token
```

- Every request: `redirect: "error"`, `Authorization: Bearer <token>`, `AbortSignal.timeout` (health 2 s, JSON GET 10 s, POST 30 s, approval 15 s, transcribe 150 s). Every token-bearing request is preceded by `assertListenerOwner` when `expectedListener` is set (3.8.1); the process proxy variables are neutralized at startup (3.11.4).
- `connect(signal)`:
  1. `GET /api/health` (2 s); network error → `unreachable`.
  2. `assertListenerOwner` (3.8.1), then `GET /api/chat/events` with `Accept: text/event-stream`. 401 → `token.reload()` and one retry, then `auth` (fatal). The hint depends on `token.kind()` (UX-10): `client` → "this tenant may predate client tokens: re-run `sudo ic/scripts/lunarwing-mt-admin.sh tui-access <t> --for-user $USER --use-master-token`, or `upgrade-tenant <t>`"; `master` → "the master token changed: re-run `tui-access <t> --for-user $USER --use-master-token`"; `file` → "the token in <file> was rejected". 403 → `forbidden` (fatal; client tokens are route-scoped); 503 "Too many connections" → `busy`, retry after 30 s. Events are buffered until step 6. One SSE connection per tenant; filtering by thread is client-side.
  3. Thread: `last` → id from the state file, verified with `GET /api/chat/history?thread_id=<id>&limit=1` (400/404 → `new`); `new` → `POST /api/chat/thread/new`; `assistant` → `GET /api/chat/threads` `.assistant_thread.id`; `id` → verified like `last`. The id is written back. Default `last` (a TUI-owned thread, open question A.1).
  4. `GET /api/chat/capabilities` (404 → legacy), then `GET /api/voice/status` (not rate-limited; the voice rate-limit slot is consumed before the configuration check, `ic/src/channels/web/voice.rs:455-472`, so status is always checked first).
  5. History seed `GET /api/chat/history?thread_id=<id>&limit=30` → sealed `history` records, `pending_approval` → prompt; `GET /api/chat/pending-gates` filtered to the thread.
  6. Return `ServerInfo`; release buffered events; a buffered `response` equal to the newest history response is dropped as a duplicate.
- SSE reader: `fetch` body → `SseParser` (`transport/sse.ts`, built on `LineFramer`, `maxEventBytes = 24 MiB`) → `JSON.parse` → type guard → mapper. Watchdog: 45 s without any byte (three missed 15 s keepalives, `ic/src/channels/web/sse.rs:245`) aborts and reconnects. Oversized events and JSON errors are protocol errors.
  - WHATWG rules: blank line dispatches; `:` comment calls `onActivity`; `field: value` strips one space; multiple `data:` lines join with `\n`; `id`/`retry` ignored; `\n` or `\r\n`.
- `send()`: body as in 4.2.4. Pre-flight `sum(4*ceil(n/3)) + 6*contentBytes + 2 KiB <= maxRequestBytes - 64 KiB`, else `too_large` without I/O. Status: 202 ok; 400 `bad_request`; 401 reload once then `auth`; 403 `forbidden`; 413 `too_large`; 422 `bad_request` (G5 text names the image index); 429 `rate_limited` (retry after 60 s); 503 `unavailable`; other 5xx `server`. On failure the session controller restores the draft as described in 3.10.5.
- `interrupt()` / `steer()`: `POST /api/chat/send` with `/interrupt` or `/steer <text>` and `client_turn_id = requestId`. `approve()`, `authToken()`, `authCancel()` as in 3.6.
- `resync()`: history (limit 30) + pending-gates → `ResyncSnapshot`. Matching: a history turn matches an open local turn when `started_at >= sentAt - 5 s` and `user_input` starts with the sent text (it may carry an `<attachment>` block, `ic/src/bridge/router.rs:3823-3825`). A terminal state with a response seals it (`recovered:true`). A `Failed` turn without a response does not seal a local turn younger than 610 s (durable V2 history shows in-flight turns as failed; the late watcher may run another 600 s).
- `transcribe()`: `POST /api/voice/transcribe {"audio":<b64>,"format":"wav"}`; `language` omitted (the sidecar treats the gateway's `auto` as its configured language; a mismatched explicit language is a 400). Error mapping in 5.3.1.
- `close()`: aborts the SSE fetch and in-flight requests.

#### 3.8.1 Listener ownership (`tui/src/transport/listener-owner.ts`, SEC-01)

Tenant gateways listen on `0.0.0.0:<registry port>` under the tenant uid, a port above 1024 (this host: `00000000:2710` owned by uid 1010 `derpy`, `:271A` uid 1009, `:2724` uid 1011, `:272E` uid 1017, `:2742` uid 1008, from `/proc/net/tcp`). While a daemon is not holding its port (restart, upgrade, crash, `stop-tenant`; the unit restarts after 5 s, `ic/scripts/mt-admin-lib/07-units-systemd.sh:1049-1050`) any other uid can bind it, and the reconnecting TUI would send its bearer token to that process. The client therefore checks who owns the listener before every token-bearing request.

```ts
export interface ListenerRow { family: 4 | 6; address: string; port: number; uid: number }
export type ProcReader = (path: "/proc/net/tcp" | "/proc/net/tcp6") => Promise<string>
/** Rows in state 0A (LISTEN) only. IPv4 addresses are 8 hex digits in little-endian byte order
 *  (0100007F = 127.0.0.1); IPv6 addresses are four 32-bit little-endian words
 *  (00000000000000000000000001000000 = ::1, 0000000000000000FFFF00000100007F = ::ffff:127.0.0.1). */
export function parseProcNetTcp(text: string, family: 4 | 6): ListenerRow[]
export async function listenerUids(url: URL, read?: ProcReader): Promise<number[]>
export async function assertListenerOwner(url: URL, expected: { uid: number; label: string }, read?: ProcReader): Promise<void>
```

- `read` defaults to `node:fs/promises` `readFile(path, "utf8")` (procfs reports size 0, so streaming readers that trust `stat` must not be used). Columns: `sl local_address rem_address st tx:rx tr:tm retrnsmt uid ...`; the local address is column 2, the state column 4, the uid column 8.
- Matching rows: the URL's port, and a local address equal to the URL host, the wildcard (`0.0.0.0`, `::`), or the v4-mapped form of the URL's IPv4 host; `localhost` matches both `127.0.0.1` and `::1`.
- No matching row → `TransportError("unreachable", "nothing listens on 127.0.0.1:10000")`, retried, nothing sent. Any matching row with another uid → `TransportError("forbidden", "port 10000 is held by uid 1008, expected 1010 (derpy)", {fatal: true})`.
- Expected uid: tenant profiles use `tenant_uid` from the tenant file (9.5), which is required (a file without it is refused with "re-run tui-access"); tenant gateways always run on this host, so the check applies whatever the URL host is. Other gateway profiles with a loopback URL (127.0.0.0/8, `::1`, `localhost`) use `expected_uid` from the profile or `process.getuid()`. Other URLs are not checked (sticky warning; `https` recommended). The Hex sources use `process.getuid()` (5.3.2).
- Residual race (documented, accepted): the listener can change between the check and the connect. The window is one `readFile` plus one connect, and a reconnect loop cannot be steered into it reliably.
- Tests (`tui/test/transport/listener-owner.test.ts`, fixtures `tui/test/fixtures/proc-net/{tcp-tenants,tcp6-loopback,tcp-foreign}.txt` copied from this host's format): IPv4 loopback and wildcard rows, IPv6 `::1`, wildcard and v4-mapped rows, a foreign-uid row, no row; and in `gateway-transport.test.ts` "a listener owned by another uid never receives an Authorization header" (injected `ProcReader`).

### 3.9 SocketTransport (`tui/src/transport/socket.ts`, M4)

- Path order: `--socket`, `LUNARWING_SOCKET`, `IRONCLAW_SOCKET`, `$XDG_RUNTIME_DIR/lunarwing.sock`, `$LUNARWING_BASE_DIR` (or `$IRONCLAW_BASE_DIR`)`/lunarwing.sock`, `~/.ironclaw/lunarwing.sock` (mirrors `ic/src/main.rs:490-500`). Before connecting: `lstat` must show a socket (not a symlink) owned by `process.getuid()`, else `forbidden` (fatal). `ENOENT`/`ECONNREFUSED` → `unreachable` (retry). `EACCES` → `permission_denied` (fatal) with the hint "tenant sockets are owner-only; use `--tenant <t>` after `tui-access`".
- Framing (`transport/framing.ts`, shared with SSE): split on byte `0x0A` and decode each line with `TextDecoder("utf-8").decode(seg, {stream:true})`, flushing at line end (fixes C4); linear in input (fixes C6); bounded at 32 MiB until `Welcome` sets `max_frame_bytes`; an oversize line throws `FrameTooLargeError`, destroys the socket and reconnects.

```ts
export class LineFramer {
  constructor(maxLineBytes: number)
  push(chunk: Uint8Array): string[]   // complete lines without "\n" / "\r\n"; throws FrameTooLargeError
  reset(): void
}
```

- Handshake: write exactly one line `{"Connect":{"version":"wingtip/<ver>","client_info":"opentui/0.8.1 bun/<ver>","protocol":3,"features":["status","attachments","transcribe","stream"],"thread":"tui-<profile>-<16 hex>"}}`. Wait up to 5 s for the first line (the server allows 10 s for `Connect`); on timeout destroy the socket (fixes C11).

| First frame | Classification | Action |
|---|---|---|
| `{"Welcome":{...}}` | REPLv3 | apply features and limits; ready |
| `{"Error":{"code":"server_busy","fatal":true}}` | busy | backoff |
| `{"Error":{"code":"thread_in_use","fatal":true}}` | another live connection holds this thread token (4.3.3) | backoff; offline after 3 with "thread in use by another window; /thread new" |
| other fatal `Error` | refusal | backoff; offline after 3 identical refusals |
| `{"Response":{"content":"Server busy: ..."}}` | busy (v1, `ic/src/channels/unix_socket_repl.rs:163-178`) | backoff |
| `{"Response":{"content":"Connected to ..."}}` | legacy v1 | legacy capabilities, warning banner |
| anything else, EOF, timeout | protocol error | destroy, backoff |

- The thread token is minted per process (F3): `tui-<profile>-<16 hex>` from `crypto.getRandomValues`, reused only for this process's reconnects. It is written to the profile state file as `lastReplThread` only so that `--thread last` or `/thread resume` can rejoin it deliberately; the default for the socket is a new thread per launch. `/thread new` rotates it and reconnects. The daemon refuses a second live v3 connection on the same token (`thread_in_use`), so two windows never share a socket thread.
- Write path: one serialized queue; frame = `JSON.stringify(f) + "\n"`; frames over `maxRequestBytes` are refused locally; `write()` returning `false` waits for `drain`. A missing `Ack` after 10 s shows a warn notice (no resend).
- Keepalive: `"Ping"` every 30 s (inside the server's 180 s idle timeout); two missed `"Pong"` (10 s each) destroy the socket; server `"Ping"` is answered.
- Never sends a frame or field of a feature missing from `Welcome.features`; content over `max_content_bytes` UTF-8 bytes is refused locally.
- Loss: REPLv3 reconnects with the same thread token and gets pending `ApprovalRequest`s and stashed frames replayed; legacy turns become `lost`.
- `transcribe()`: `Transcribe{req_id:"t-"+uuid, format:"wav", audio}`; resolves on `TranscribeResult{req_id}`, rejects on `Error{req_id}`, on abort (late result ignored) or after 150 s; one in flight per connection.

### 3.10 Commands, keys and guards

#### 3.10.1 Local slash commands (`tui/src/controller/commands.ts`)

The composer text is trimmed; a leading `/` starts a command; the first token matches case-insensitively.

| Command | Local action | Sent to the daemon |
|---|---|---|
| `/quit`, `/exit` (any args) | quit: supervisor stop, transport close, voice and children disposed, `renderer.destroy()` | never |
| `/shutdown` | refused: "not forwarded: /shutdown stops the daemon. For tenants use `lunarwing-mt-admin.sh stop-tenant <t>`." | never |
| `/restart` | refused with "use `lunarwing-mt-admin.sh restart-tenant <t>`"; `//restart` forwards after a second Enter within 5 s (R17) | only via `//restart` |
| `/help`, F1 | opens the help panel (3.12.3) | no (`//help` forwards) |
| `/img <path>`, `/attach <path>`; `/shot [full]` | section 6 | no |
| `/image open [n]`, `/image save [n] [path]` | open or save the newest (or n-th newest) generated image (6.4) | no |
| `/copy [n]` | copy the final text of the newest (or n-th newest) reply (3.12.4) | no |
| `/voice [status]` (voice panel), `/voice use <id>`, `/voice auto`, `/voice order <ids>`, `/voice device` (devices panel), `/voice device <name>` (select and persist, 3.11), `/voice test`, `/voice privacy` (privacy panel), `/voice retry` (5.4.1) | section 5 | no |
| `/transport [status]`, `/transport reconnect` | transport panel: endpoint (no token), listener owner, protocol, engine, thread, capabilities, limits, warnings; reconnect now | no |
| `/interrupt`, `/stop`, `/steer <note>` | 3.6.1 | as mapped |
| `/approve`, `/always`, `/deny`; `/approval view` | answer the focused prompt (3.6.2 rules); open its full view | structured, or text on legacy |
| `/thread`, `/thread list` (threads panel), `/thread new`, `/thread <uuid prefix>`, `/thread resume` (socket: rejoin `lastReplThread`) | gateway threads API; REPLv3 token rotation | legacy socket: forwarded verbatim |
| `/clear`, `/new` | forwarded verbatim and recorded as controls of kind `clear` / `new` (3.5) | yes |
| `//<text>` | sends `/<text>` through the guards | yes |
| any other `/cmd` | forwarded verbatim (`/compact`, `/undo`, `/model`, `/status`, ...) | yes, through the guards |

#### 3.10.2 Send-path guard (`tui/src/transport/guards.ts`)

```ts
const FORBIDDEN_CONTENT = new Set(["/quit", "/exit", "/shutdown"])
export function isForbiddenContent(content: string): boolean {
  return FORBIDDEN_CONTENT.has(content.trim().toLowerCase())   // same normalization as SubmissionParser (ic/src/agent/submission.rs:125-127)
}
```

Checked by `controller/session.ts` before every send and steer, and asserted again in both transports' write paths (a violation throws `TransportError{bad_request}` and never reaches the wire). This covers `//quit`, pasted text and dictated transcripts.

#### 3.10.3 Approval keywords, typed or dictated (SEC-08, UX-07)

`tui/src/controller/approval-reply.ts`:

```ts
/** NFKC, lower-case, trim, strip leading and trailing whitespace and . , ! ? ; : ' " ` characters, collapse inner
 *  whitespace, then match the daemon's keyword list (ic/src/agent/submission.rs:179-199) plus "okay". */
export function matchApprovalReply(text: string): ApprovalDecision | null
/** The rules of 3.6.2 for the focused prompt. */
export function approvalGate(prompt: ApprovalPrompt, composer: ComposerState, width: number, now: number):
  "ok" | "too_new" | "typed_before_prompt" | "hidden_unviewed" | "truncated"
```

On Enter, when `matchApprovalReply(composerText)` is not `null`:

| Situation | Behaviour |
|---|---|
| a prompt is pending on the current thread, decision `approve` or `deny`, `approvalGate` returns `ok` | nothing is sent; `approval/confirm_requested`; the panel title reads "Enter: approve `<tool>` - Esc: back to editing" (or deny); a second Enter, or Alt+Y / Alt+N, within 10 s applies it through `approve()` and clears the composer; Esc or any edit cancels the confirmation |
| a prompt is pending, decision `always` | nothing is sent; notice "use Alt+A or /always to approve permanently"; typed text never yields `always` |
| a prompt is pending, any other gate result | nothing is sent; notice "approval pending: Alt+Y approve, Alt+N deny, Alt+A always (or /approve, /deny, /always)" plus the reason |
| nothing pending | the first Enter shows "'ok' is an approval keyword; with nothing pending the agent swallows it. Press Enter again to send anyway."; a second Enter within 10 s sends it. Current gateways then answer `turn_finished{no_reply}`, REPLv3 answers `Error{no_pending_approval}`, legacy daemons time out as `unanswered` after 15 s |

While a prompt is pending a bare keyword is never sent as chat, because the daemon would take it as the decision for whatever approval is pending on that thread (`submission.rs:179-199`, `thread_ops.rs:1405-1422`). Punctuated forms such as `Yes.` (what Parakeet returns) do not match on the daemon side (`submission.rs:15-16`, `:179-199`), so the confirmation step is how voice answers an approval. Text that only starts with a keyword (`Yes, but use staging.`) is not a reply and is sent normally.

#### 3.10.4 Keys (`tui/src/controller/keymap.ts`, one global `useKeyboard(handler, {release: true})`, `otr081:src/hooks/use-keyboard.d.ts`)

`keymap.ts` exports the table below as data (`KEYMAP: { keys: string[]; action: string; when: string }[]`); the help panel and the footer are generated from it.

| Key | Action |
|---|---|
| Enter | composer `submit` (3.10.5): send; in confirm state, apply the confirmed decision; a second Enter confirms a guarded send; in credential mode, submit the credential |
| Ctrl+J, Alt+Enter, Shift+Enter (KKP) | newline in the composer |
| Ctrl+S (Ctrl+Enter with KKP) | interrupt, then send; interrupt only when the composer and the attachments are empty (3.6.1) |
| F9, Ctrl+Space (NUL inside tmux) | push-to-talk (section 7) |
| F1 | help panel |
| Esc | the first that applies: close the info panel; cancel a pending confirmation; cancel dictation (the recording, else the transcription queue, else the retained clip); in credential mode arm, then cancel (3.6.4); dismiss the newest notice |
| Ctrl+V | clipboard paste (image, file list or text) |
| Ctrl+R / Ctrl+X | remove last attachment / clear attachments |
| Alt+Y / Alt+A / Alt+N | approve / always / deny the focused approval (only while one is pending, under the rules of 3.6.2; otherwise Alt+A is the textarea's visual-line-home) |
| Alt+V / Alt+] | full approval view / next approval |
| Alt+R | retry the retained voice clip (5.4.1) |
| Up / Down | on the first / last visual line of the composer: previous / next history entry; elsewhere cursor movement |
| Ctrl+_ (what Ctrl+- sends in legacy encoding, including inside tmux) / Ctrl+. | undo / redo in the composer |
| PageUp / PageDown | scroll the info panel when it is open, else the transcript; reaching the end re-enables follow |
| Ctrl+C | cancel dictation; else clear a non-empty composer (undoable, the draft goes to history); else "press Ctrl+C again to quit" (2 s) |

Every handled key calls `preventDefault()`; global handlers run before the focused renderable (`ot081:chunk-bun-ved2tpxm.js:289-330`), so F9 and NUL never reach the composer. Keys the global handler does not claim go to the focused textarea with the bindings of 3.10.5.

#### 3.10.5 Composer (`tui/src/components/composer.tsx`, `tui/src/controller/composer-handle.ts`, UX-01)

The composer is OpenTUI's `<textarea>` (`otr081:src/components/index.d.ts:12`; `TextareaRenderable`, `ot081:renderables/Textarea.d.ts`). The single-row `<input>` is not used: its height is always 1, it does not wrap and it strips newlines (`ot081:renderables/Input.d.ts:24-27`, `ot081:index.bun.js:8891-8892`), so a 120 s dictation could only be reviewed through a 70-column window and pasted lines merged.

```tsx
<textarea ref={textareaRef} wrapMode="word" keyBindings={COMPOSER_KEY_BINDINGS}
  onSubmit={() => session.submit()} onContentChange={() => composer.reportMetrics()}
  cursorStyle={{ style: "block", blinking: config.ui.cursorBlink }} placeholder={hint}
  style={{ height: Math.min(Math.max(lines, 1), 6), flexGrow: 1 }} />
```

```ts
// controller/composer-handle.ts. OpenTUI merges these over defaultTextareaKeyBindings, keyed by name + modifiers, so
// they replace the defaults return -> newline and kpenter -> newline (ot081:index.bun.js:8540-8605, :8616-8650;
// mergeKeyBindings, ot081:chunk-bun-ved2tpxm.js:17578-17592).
export const COMPOSER_KEY_BINDINGS: KeyBinding[] = [
  { name: "return", action: "submit" },
  { name: "kpenter", action: "submit" },
  { name: "return", shift: true, action: "newline" },   // KKP only
  { name: "return", meta: true, action: "newline" },    // Alt+Enter (ESC CR), also inside tmux
  { name: "linefeed", action: "newline" },              // Ctrl+J (LF), also inside tmux
  { name: "_", ctrl: true, action: "undo" },            // 0x1F parses as ctrl+"_" (chunk-bun-ved2tpxm.js:3836-3846)
]
```

`ComposerHandle` (interface in `controller/types.ts`, implementation in `composer-handle.ts`): `getText()` (`plainText`), `insertText(text)`, `replaceText(text)` (undoable, `ot081:renderables/EditBufferRenderable.d.ts:232`), `clear()` (`replaceText("")`), `focus()`, `blur()`, `visualRow()` and `visualLines()` (`visualCursor.visualRow`, `virtualLineCount`, `EditBufferRenderable.d.ts:95-99`), `reportMetrics()` (dispatches `composer/metrics` with lines and UTF-8 bytes).

Rules:
- Height: `clamp(virtualLineCount, 1, 6)` text rows plus the border; `computeLayout` takes `composerLines` (3.12.1) and the transcript gives up the rows. Longer text scrolls inside the textarea.
- Pastes keep their newlines (`TextareaRenderable.handlePaste`, `ot081:index.bun.js:8715-8717`); drag-and-drop detection stays in the global `usePaste` (6.1).
- Size: `limits.maxContentBytes` is enforced at send on the UTF-8 byte length; above 80 % the border title shows `N KiB / 98 KiB`. Nothing is truncated while typing.
- `composer.startedAt` is set by the first edit or insert while the composer is empty and cleared when it becomes empty (used by 3.6.2 and 3.10.3).
- History: sent texts and cleared drafts are pushed to `composer.history` (in memory only, at most 50, never written to disk). Up on visual row 0 and Down on the last visual row recall entries.
- Ctrl+C on a non-empty composer pushes the draft to history and calls `clear()`, so Ctrl+_ restores it.
- Failed send (`turn/send_failed`): if the composer is still empty, the text is restored with `replaceText` and the images are re-attached; if the user has typed since, the failed text goes to history with the notice "send failed: text moved to history (Up)". Images that no longer fit are reported, never dropped silently.
- Dictation inserts at the cursor with `insertText`, adding a separating space after a non-space character.
- In credential mode the textarea is blurred and receives nothing (3.6.4).

### 3.11 Configuration and secrets (`tui/src/config/*`)

Precedence: flags > environment > config-file profile > config-file globals > defaults. Profile names must match `^[a-z0-9_-]{1,32}$` (R25). A tenant chosen without a profile uses the tenant name as its profile name (tenant names are `[a-z0-9-]`).

#### 3.11.1 Flags and environment

| Flag | Meaning |
|---|---|
| `--tenant <name>` | gateway URL, token and listener uid from `tenants/<name>.json` (section 9) |
| `--profile <name>`, `--config <path>` | profile and config file |
| `--transport gateway\|socket\|auto` | transport (`auto`: 3.11.5) |
| `--gateway-url <url>`, `--token-file <path>` | non-tenant gateway |
| `--socket <path>` | socket path (M4) |
| `--thread last\|new\|assistant\|<uuid>` | gateway default `last` (a TUI-owned thread, A.1); socket default `new`, where `last` rejoins `lastReplThread` (3.9) |
| `--image-protocol auto\|kitty\|sixel\|blocks` | kept from vibetui4 |
| `--voice-order <ids>`, `--no-voice`, `--ptt-key <key>` (repeatable) | voice |
| `--debug-log [<path>]` | metadata-only debug log |
| `--token`, `--gateway-token`, `--token=<v>` | **rejected**, exit 2: "tokens are not accepted on the command line (visible in ps and /proc)" |

Environment: `LUNARWING_TUI_CONFIG_DIR`, `LUNARWING_TUI_CONFIG`, `LUNARWING_TUI_PROFILE`, `LUNARWING_TUI_TRANSPORT`, `LUNARWING_GATEWAY_URL`, `LUNARWING_GATEWAY_TOKEN_FILE`, `LUNARWING_GATEWAY_TOKEN` (read once, then blanked in `process.env`), `LUNARWING_SOCKET`/`IRONCLAW_SOCKET`/`XDG_RUNTIME_DIR`/`LUNARWING_BASE_DIR`/`IRONCLAW_BASE_DIR`, `LUNARWING_TUI_IMAGE_PROTOCOL`, `LUNARWING_TUI_DEBUG_LOG`, `LUNARWING_TUI_HEX_BIN`, `HEX_APPLICATION_SUPPORT_DIR`, `NO_COLOR` (3.12.2). Because Bun keeps its own copy of the environment, every variable the client wants gone is assigned `""`, never deleted (3.11.4).

#### 3.11.2 Config file

Config directory search order: `$LUNARWING_TUI_CONFIG_DIR`, `${XDG_CONFIG_HOME:-$HOME/.config}/wingtip`, `$HOME/.config/wingtip` (the last is where `tui-access` writes). Optional `config.toml` (parsed with `Bun.TOML.parse`; refused unless owned by the user and `(mode & 0o022) === 0`, because it names commands):

```toml
default_profile = "derpy"
[ui]
image_protocol = "auto"
max_turns_in_memory = 500
color = "auto"                   # auto | always | never; auto honours NO_COLOR (3.12.2)
reduce_motion = false            # no spinner, VU meter at 4 Hz
cursor_blink = true
notify = "approvals"             # approvals | all | off (3.6.2)
vision_verified_tenants = []     # tenants where supervised test 16.5 passed (6.5)
[voice]
order = ["lunarvoice", "hex-app", "hex-embedded"]
ptt_keys = ["f9", "ctrl+space"]
repeat_delay_ms = 300            # Hyprland repeat_delay on this host (default 600)
max_duration_s = 120             # hard cap 290
auto_stop_on_silence = false     # toggle mode only, opt-in (7.2)
silence_stop_ms = 3500
blur_policy = "hold"             # hold | always | never
# device = "C922"                # /voice device <name> stores a per-profile choice instead (3.11.6)
[voice.hex]                      # M1b
command = ["hex"]
model = ""                       # empty: first verified model from GET /models
[profiles.derpy]
transport = "gateway"
tenant = "derpy"                 # url, token and tenant_uid from tenants/derpy.json; nothing else is read for tenants
thread = "last"
[profiles.devgw]
transport = "gateway"
gateway_url = "http://127.0.0.1:8765"
token_file = "~/.config/wingtip/tokens/devgw.token"
# expected_uid = 1000            # listener owner for a loopback URL; default: the current uid (3.8.1)
[profiles.dev]
transport = "socket"
socket = "/run/user/1000/lunarwing.sock"
```

#### 3.11.3 Tokens and URLs

Token sources:
- **Tenant access file** `tenants/<t>.json` (section 9.5), the only source for tenant profiles. Before reading the token: `lstat` regular file, owner `process.getuid()`, `(mode & 0o077) === 0`, size ≤ 4 KiB, `schema == "wingtip/tenant-access"`, `version == 1`, `expires_at` not past, URL valid, `tenant_uid` an integer > 0 (required, SEC-01; a file without it is refused with "re-run tui-access"). Re-read once on 401. `token_kind:"master"` adds a sticky warning.
- **Non-tenant profiles**, in order: `--token-file`, `LUNARWING_GATEWAY_TOKEN_FILE`, `LUNARWING_GATEWAY_TOKEN`, profile `token_file`, `<config dir>/tokens/<profile>.token`. Token files are opened `O_RDONLY|O_NOFOLLOW`; `fstat` regular, owner uid, `(mode & 0o077) === 0`, ≤ 4096 bytes; trimmed content `^[\x21-\x7e]{16,512}$`. Failures name the file and the check, never the content.
- `config/secret.ts`: `class Secret { expose(): string }` with `toString`, `toJSON`, `Bun.inspect.custom` and `nodejs.util.inspect.custom` returning `[redacted]`; `expose()` is called only where the `Authorization` header is built.
- URL rules: `http` or `https`; no userinfo, query or fragment; plain `http` only to loopback (127.0.0.0/8, `::1`, `localhost`) unless `allow_insecure_http = true` (sticky warning); redirects refused. Loopback and tenant URLs get the listener-owner check (3.8.1).
- `config/child-env.ts`: children (parec, wl-paste, wl-copy, grim, slurp, notify-send, xdg-open, hex) get only `PATH`, `HOME`, `LANG`, `LC_*`, `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `WAYLAND_DISPLAY`, `DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`, `PULSE_SERVER`, `PIPEWIRE_RUNTIME_DIR`, `HEX_APPLICATION_SUPPORT_DIR`; `TRANSCRIBE_DUMP_DIR` and the proxy variables are never passed.

#### 3.11.4 Proxy environment (`tui/src/config/proxy-env.ts`, SEC-14)

Bun's `fetch` sends requests through `HTTP_PROXY`, `HTTPS_PROXY` or `ALL_PROXY` whenever the `proxy` option is unset, loopback requests included, and bypasses only hosts listed in `NO_PROXY` (`vibetui4-tui/tui/node_modules/bun-types/globals.d.ts:2014-2051`, `bun.d.ts:8981-8988`). Every peer of the TUI is local or explicitly configured, so the process disables proxying before its first `fetch`:

```ts
export const PROXY_ENV_NAMES = ["http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"] as const
/** Called first in main.tsx. Assigning (not deleting) is what works on Bun 1.3.14. */
export function neutralizeProxyEnv(env: Record<string, string | undefined> = process.env): void {
  for (const name of PROXY_ENV_NAMES) env[name] = ""
  env.NO_PROXY = "*"
  env.no_proxy = "*"
}
```

Measured on the host's Bun 1.3.14 with a loopback recording proxy (`probe:proxy-probe.ts`, `proxy-probe2.ts`, `proxy-probe3.ts`): with `HTTP_PROXY` inherited from the parent, requests to `127.0.0.1`, `localhost` and `[::1]` all went to the proxy; `delete process.env.HTTP_PROXY`, `proxy: false` and `proxy: ""` did not change that; assigning `""` to the six names, or setting `NO_PROXY="*"`, sent all three directly; `NO_PROXY="127.0.0.1,localhost,::1"` still proxied `[::1]`. The test `tui/test/config/proxy-env.test.ts` spawns `tui/test/fixtures/proxy-child.ts` with `HTTP_PROXY` set to a recording proxy and asserts that the proxy sees zero requests and the mock gateway sees all of them, so a Bun upgrade that changes this behaviour fails CI.

#### 3.11.5 Tenant and transport selection (`tui/src/config/tenant-select.ts`, UX-10)

`listTenantFiles(configDir)` returns the valid `tenants/*.json` files (validated as in 3.11.3) and the invalid ones with their reason. Resolution:
1. `--tenant`, a profile naming a tenant, `--gateway-url` or `--socket`: use it. An explicit `--transport` or profile `transport` wins over the rest of this list.
2. Exactly one valid tenant file: use it.
3. Several: `components/tenant-picker.tsx` renders an OpenTUI `<select>` (`otr081:src/components/index.d.ts:11`; `SelectOption{name, description}`, `ot081:renderables/Select.d.ts:9-12`; `onSelect`, `otr081:src/types/components.d.ts:62`) listing tenant, URL, `token_kind` and expiry, preselecting `lastTenant` from `state.json`; Enter picks, Esc quits. The pick is written back as `lastTenant`.
4. None: a user-owned socket at the resolved path (M4) selects the socket; otherwise exit 2 with "no tenant access is configured for <user>. An operator can grant it with: sudo ic/scripts/lunarwing-mt-admin.sh tui-access <tenant> --for-user <user> --use-master-token (today's daemons; drop --use-master-token once the tenant runs M2b)". The client never reads the ports registry (R14), so it does not list tenants.

Instance lock (F3): `config/instance-lock.ts` exports `acquireProfileLock(stateDir: string, profile: string): Promise<{ primary: boolean; release(): Promise<void> }>`. It creates `<state>/<profile>.lock` with `O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW`, mode 0600, holding the pid; an existing lock whose pid is dead (`process.kill(pid, 0)` throws `ESRCH`) is replaced. A secondary instance (a live pid holds the lock) never writes `lastThreadId`, `lastReplThread` or `lastTenant`, uses a new gateway thread unless `--thread` was given, and shows "second window" in the header. The lock is released on quit.

#### 3.11.6 Files written by the client

| Path | Mode | Content |
|---|---|---|
| `${XDG_STATE_HOME:-~/.local/state}/wingtip/` | 0700 | directory |
| `.../state.json` | 0600 | `{"lastTenant":"derpy"}` |
| `.../<profile>.json` | 0600 | `{"lastThreadId":"<uuid>","lastReplThread":"tui-<profile>-<16 hex>","voiceAck":{"lunarvoice":"2026-11-03T..."},"imageAck":{"clipboard":"2026-11-03T..."},"voiceDevice":"C922"}` |
| `.../<profile>.lock` | 0600 | pid of the primary instance |
| `.../debug.log` (only with `--debug-log`) | 0600 | JSON lines of event types, sizes, durations and status codes; never message text, transcripts, audio, image bytes, headers or tokens |
| `$XDG_RUNTIME_DIR/wingtip/` | 0700 | `/image open` exports, removed on quit (6.4) |

State files are written atomically (`<file>.tmp` opened `O_CREAT | O_EXCL | O_NOFOLLOW`, mode 0600, then `rename`). The app never calls `console.log` (OpenTUI owns the terminal).

### 3.12 Layout, rendering and sanitization

#### 3.12.1 Layout (`state/layout.ts`, `app.tsx`)

- `computeLayout({width, height, notices, approvalPending, pendingImages, composerLines}) -> {header:1, transcript>=3, approval:0|3|8, notices:0|1|2, attachments:0|1|9, composer:3..8, footer:0|1, imageColumns}`. The composer takes `clamp(composerLines, 1, 6) + 2` rows (3.10.5), capped at 3 text rows below 16 terminal rows. Footer dropped when `height < 10`; approval 8 rows (3 when `height < 14`); notices `min(count,2)` (1 when `height < 14`); attachments 9 rows when `height >= 16`, else 1; `transcript = max(3, height - others)`; when rows still do not fit: drop the footer, compact approvals and attachments, cut notices to 1, then shrink the composer to 1 text row.
- The composer row is `[mic widget, 10 columns][VU meter, 12 columns, only while recording][textarea]` (7.4).
- `app.tsx` renders `<box style={{width, height, flexDirection:"column"}}>` whose children all have explicit `height` and `flexShrink: 0`, with one `<scrollbox stickyScroll stickyStart="bottom" scrollX={false} style={{height: layout.transcript, flexShrink: 0}}>` holding every `TurnView`, the live one included (fixes C5). Dimensions come from `useTerminalDimensions()` (fixes C8). PageUp/PageDown use `scrollBy(±(transcript - 2))`. The info panel (3.12.3) and the tenant picker (3.11.5) are absolutely positioned overlays and take no rows.
- An empty thread shows a welcome block: tenant, transport and engine, "exact" or "legacy daemon", the PTT keys for the detected environment, and "F1 help".

#### 3.12.2 Theme and accessibility (`components/theme.ts`, UX-15)

- `resolveTheme({ color: config.ui.color, noColor: process.env.NO_COLOR }): Theme`. `auto` turns colour off when `NO_COLOR` is set and non-empty (OpenTUI does not honour it itself); `never` always; `always` ignores `NO_COLOR`. The monochrome theme uses only bold, inverse and underline.
- Every state carries a text label, never colour alone: mic `( mic )`, `( ... )`, `REC 0:07`, `[ .... ]`, `(no mic)`; header `OFFLINE`, `legacy`, `second window`; approval `APPROVAL`; turn phases as words.
- `[ui] reduce_motion = true`: no spinner animation, VU meter redrawn at 4 Hz instead of 15 Hz. `[ui] cursor_blink = false` sets the textarea's `cursorStyle.blinking` (`ot081:renderables/EditBufferRenderable.d.ts:84-87`, `ot081:types.d.ts:39-44`).

#### 3.12.3 Info panel, help and footer (`components/info-panel.tsx`, `components/footer.tsx`, UX-04)

- The info panel is a `<box>` with `position: "absolute"` and `zIndex: 10` (`ot081:Renderable.d.ts:41`, `:70`), `top: 1`, `left: 2`, width `view.width - 4`, height `view.height - 4`, a border titled with the panel kind, holding a `<scrollbox>`. `AppState.panel = {kind, scroll, approvalId?}` with actions `panel/open`, `panel/scroll`, `panel/close`. Content comes from the pure `controller/panel-content.ts` `buildPanel(kind: PanelKind, state: AppState, env: PanelEnv): string[]`, so live panels (voice) follow the state.
- Kinds and openers: `help` (F1, `/help`); `voice` (`/voice`, `/voice status`: every source with availability, reason and last attempts, the recorder, the device, the retained clip); `privacy` (`/voice privacy`: the 5.6 table with the active backend marked); `devices` (`/voice device`); `transport` (`/transport`); `threads` (`/thread list`); `approval` (Alt+V, `/approval view`, 3.6.2).
- Help is generated from `KEYMAP` (3.10.4) and the environment, for example inside tmux: "tap F9 to start and stop; holding F9 also works (key repeat); or hold the mic with the mouse". Esc closes the panel before anything else; PageUp/PageDown scroll it.
- Footer: one row, `selectable={false}`, text from the pure `controller/footer-hint.ts` `footerHint(state: AppState): string`; the first matching row wins:

| State | Footer |
|---|---|
| offline | `offline: <reason> - /transport reconnect - F1 help` |
| credential mode | `Enter send credential - Esc Esc cancel` |
| confirmation pending (3.10.3) | `Enter confirm - Esc back` |
| approval pending | `Alt+Y approve - Alt+N deny - Alt+A always - Alt+V view - Alt+] next` (Alt+A only with `allowAlways`, Alt+] only with more than one prompt) |
| recording, toggle | `REC 0:07 - F9 stop - Esc cancel` |
| recording, hold | `REC 0:07 - release to stop - Esc cancel` |
| transcribing | `transcribing - Esc cancel` |
| turn running | `Enter queue - Ctrl+S stop - /steer <note> - F1 help` |
| idle | `F9 dictate - Ctrl+V image - /shot - Enter send - F1 help` |

#### 3.12.4 Copy (`controller/copy.ts`, UX-11)

`useMouse: true` sends mouse drags to the app, and OpenTUI only emits a `selection` event when a drag ends (`ot081:chunk-bun-ved2tpxm.js:25205-25209`); inside tmux, `set-clipboard external` makes tmux ignore OSC 52 from applications. The client copies by itself:
- `useSelectionHandler` (`otr081:src/hooks/use-selection.d.ts:12`): when a selection inside the transcript ends with a non-empty `selection.getSelectedText()` (`ot081:lib/selection.d.ts:43`), the text is written with the host clipboard instance already created for Ctrl+V: `hostClipboard.writeText(text, { selection: "clipboard" })` (`ot081:lib/clipboard.d.ts:83-89`). Status `written` → notice "copied N characters (cliphist records copies)". `unsupported`, `failed` or `timed-out` → `wl-copy` with the text on stdin (argv `["wl-copy"]`, child environment, 3 s) when `WAYLAND_DISPLAY` is set; otherwise OSC 52 through `renderer.copyToClipboardOSC52(text)` (`ot081:renderer.d.ts:613`), outside tmux only.
- `/copy` copies the final text of the newest reply; `/copy <n>` the n-th newest.
- All chrome is `selectable={false}` (header, footer, mic widget, buttons, panel frames, attachment captions), so a drag selects transcript text only. kitty's own selection (Shift+drag) is described in the README.

#### 3.12.5 Sanitization and bounds

- `sanitize.ts`: `sanitizeText(s, {multiline})` = `stripAnsiSequences` (`ot081:lib/paste.d.ts:7`), then remove U+0000-U+0008, U+000B-U+001F, U+007F-U+009F and bidi controls U+202A-U+202E, U+2066-U+2069; newlines only when `multiline`. Applied to every server string (replies, activity, approval text, notices, captions) and to transcripts. The approval panel also escapes invisible characters (3.6.2).
- Bounds: 500 records in `order`; image bytes for the newest 20 images; 50 activity entries per turn; deltas capped at 1 MiB per turn.

### 3.13 vibetui4 client defects and their fixes

| ID | Fix | Pinned by |
|---|---|---|
| C1 lost replies | per-turn records keyed by client ids; FIFO and control-ack matching only on legacy daemons | `reducer.test.ts`: "queued turn receives its own final", "interrupt ack does not close the follow-up", "final with nothing inflight becomes background" |
| C2 approvals hang | prompts and `approve()` on both transports; `turn_finished`/`TurnFinished` closes every turn | `reducer.test.ts` approval cases; `gateway-session.test.ts` approval round trip; `socket.test.ts` "TurnFinished without Response closes the turn" |
| C3 no live text on the socket | honest `capabilities.streaming`; activity and elapsed timer instead | `degrade.test.tsx` "no streaming shows activity and elapsed time" |
| C4 UTF-8 split | `LineFramer` with streaming `TextDecoder` | `framing.test.ts` "split at every byte offset" |
| C5 one-row live text | live turn inside the scrollbox | `layout-resize.test.tsx` "multi-line live text fully visible" |
| C6 O(n²) framing | scan only new bytes | `framing.test.ts` "8 MiB line in 64 KiB chunks is linear" |
| C7 `spawnSync` clipboard | async host clipboard, async `wl-paste` fallback | `clipboard.test.ts`; source grep for `spawnSync` |
| C8 no resize | `useTerminalDimensions` + `computeLayout` | `layout.test.ts`, `layout-resize.test.tsx` (WI#458 item 6) |
| C9 no reconnect | `Supervisor` with backoff; gateway resync; REPLv3 thread resume | `gateway-reconnect.test.ts`, `socket.test.ts` (WI#458 item 5) |
| C10 BMP, no limits | PNG/JPEG/GIF/WebP allowlist; server limits | `sniff.test.ts`, `limits.test.ts` |
| C11 handshake timeout, busy as welcome | socket destroyed on timeout; busy classified | `socket.test.ts` "handshake timeout destroys", "Server busy is not a welcome" |
| C12 test flaws | rewritten; `mkdtemp` + cleanup; act hygiene (11.3.5) | review checklist |

---

## 4. Transport contracts

### 4.1 Shared vocabulary (`ic/crates/lunarwing_common/src/client_caps.rs`, NEW, M2a)

`lunarwing_common` depends only on `serde` and `serde_json` (`ic/crates/lunarwing_common/Cargo.toml`) and is already a dependency of the main crate (`ic/Cargo.toml:109`). `lib.rs` gains `pub mod client_caps;` and `pub use client_caps::TurnOutcome;`.

```rust
//! Wire vocabulary shared by the gateway chat API and REPLv3 (TUI design 4.1).
use serde::{Deserialize, Serialize};

/// Max length of every client-chosen id: gateway `client_turn_id`, REPLv3 `msg_id`,
/// `req_id` and the REPLv3 thread token.
pub const CLIENT_ID_MAX_LEN: usize = 64;

/// `^[A-Za-z0-9_-]{1,64}$`. Ids are opaque: never parsed, never logged beyond the id itself.
pub fn is_valid_client_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= CLIENT_ID_MAX_LEN
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub const FEATURE_STATUS: &str = "status";
pub const FEATURE_ATTACHMENTS: &str = "attachments";
pub const FEATURE_TRANSCRIBE: &str = "transcribe";
pub const FEATURE_STREAM: &str = "stream";
/// Canonical order used whenever a server lists features.
pub const FEATURES_CANONICAL: [&str; 4] = [FEATURE_STATUS, FEATURE_ATTACHMENTS, FEATURE_TRANSCRIBE, FEATURE_STREAM];

/// How the agent finished handling one inbound message (TUI design 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// A non-empty reply was handed to outbound delivery before this event.
    Replied,
    /// Handling ended without a reply (suppressed, consumed control, stale approval, hook rejection).
    NoReply,
    /// The turn stopped at a gate (approval, credential, external callback); a continuation follows the decision.
    Paused,
    /// A running turn was stopped by `/interrupt`.
    Interrupted,
    /// The turn outlived its wait budget and continues in the background; exactly one more
    /// `Finished` follows when it ends. Not terminal.
    Detached,
    /// The handler returned an error, panicked, or the engine reported failure.
    Failed,
    /// The agent loop's soft timeout fired; the timeout notice was sent.
    TimedOut,
    /// The message never ran: removed from the queue by an interrupt, rejected by a full queue, or shutdown.
    Cancelled,
    /// Forward compatibility for readers; producers never emit it.
    #[serde(other)]
    Unknown,
}

/// Limits advertised by `GET /api/chat/capabilities` and REPLv3 `Welcome.limits`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLimits {
    pub max_content_bytes: u64,
    pub max_attachments: u32,
    pub max_attachment_bytes: u64,          // decoded bytes per image
    pub max_total_attachment_bytes: u64,    // decoded bytes per message
    pub attachment_mime_types: Vec<String>,
    pub max_transcribe_b64_bytes: u64,      // 0 when transcription is not available
    pub transcribe_formats: Vec<String>,
    pub messages_per_minute: u32,
    pub transcribes_per_minute: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_request_bytes: Option<u64>,  // gateway body
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_frame_bytes: Option<u64>,    // REPLv3 line
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_in_flight: Option<u32>,      // REPLv3
    #[serde(default, skip_serializing_if = "Option::is_none")] pub idle_timeout_secs: Option<u64>,  // REPLv3
}
```

Values per transport:

| Field | Gateway | REPLv3 |
|---|---|---|
| `max_content_bytes` | 100000 | 100000 |
| `max_attachments` / `max_attachment_bytes` / `max_total_attachment_bytes` | 5 / 5242880 / 7340032 | 5 / 5242880 / 10485760 |
| `attachment_mime_types` | png, jpeg, gif, webp | same |
| `max_transcribe_b64_bytes` | 25165824 (`MAX_AUDIO_B64_LEN`, `ic/src/channels/web/voice.rs:24`) when voice is configured, else 0 | 16777216 when `transcribe` was accepted, else 0 |
| `transcribe_formats` | `wav ogg opus mp3 mp4 webm flac m4a aac` (`voice.rs:56-58`), empty without voice | same, empty without `transcribe` |
| `messages_per_minute` / `transcribes_per_minute` | 30 / 10 per user (`ic/src/channels/web/mod.rs:125`, `:139`) | 60 / 10 per connection |
| `max_request_bytes` | 10485760 (`ic/src/channels/web/server.rs:702`) | absent |
| `max_frame_bytes` / `max_in_flight` / `idle_timeout_secs` | absent | 16842752 / 8 / 180 |

Correlation rules (both transports):
1. The client generates one id per user turn and per control (interrupt, steer) with `crypto.randomUUID()`; transcriptions use `t-<uuid>`.
2. The server validates the id with `is_valid_client_id`; an invalid id is rejected (gateway 400, REPLv3 `Error{bad_frame}`), never rewritten.
3. The server echoes the id on every event caused by that message: progress, approval prompts, image events, the reply, lifecycle events and errors.
4. An approval decision resumes the **original** turn: continuation events carry the original id. On the gateway the client sends the original `client_turn_id` with the approval; on REPLv3 the server looks it up in its per-thread pending map, and the `Approval` frame's own `msg_id` gets only the `Ack` (and an `Error` if the approval is rejected). The same holds for a credential: the gateway client sends the owning turn's `client_turn_id` with `auth-token`, and the V2 resume message carries it (8.3.1).
5. A reply without an id is out-of-band (proactive broadcasts, `message`-tool sends, job notices): the client shows it as a background item, never FIFO-matched.
6. Lifecycle: for every message the client sent, the server emits exactly one terminal finish event after any reply. `paused` is followed by `started` on resumption and another finish. `detached` is followed by exactly one more finish.

### 4.2 Gateway contract

The live handlers are in `ic/src/channels/web/server.rs`. `handlers/chat.rs`, `handlers/static_files.rs`, `handlers/settings.rs` and `handlers/extensions.rs` are compiled dead duplicates (`#[allow(dead_code)]`); do not edit them, but any struct literal of a shared type must still compile there (for that reason `SendMessageResponse` gains no fields). WebSocket (`/api/chat/ws`) is not used by the TUI: it has no rate limit and no metadata (`ic/src/channels/web/ws.rs:236-276`).

#### 4.2.1 Authentication

- `Authorization: Bearer <token>`; scheme case-insensitive; compared as a SHA-256 digest in constant time (`ic/src/channels/web/auth.rs:203-231`).
- `?token=` is accepted only for the master token on GET `/api/chat/events`, `/api/logs/events`, `/api/chat/ws` (`auth.rs:143-153`). **The TUI never uses query tokens.**
- Credentials: the master token (`GATEWAY_AUTH_TOKEN`, owner identity) and, NEW in M2b, client tokens (8.3.5): header-only, owner identity, limited to `/api/chat/*` except `/api/chat/ws`, `/api/voice/*`, and `GET /api/gateway/status`; any other protected route answers **403** `Client token not permitted for this route`. With a client token, `POST /api/chat/auth-token` also requires a pending authentication request for the named extension (403 `Client token may only answer a pending authentication request`, SEC-07).
- Error bodies: chat handlers return plain text; voice handlers return JSON `{"error","message"}`; the auth middleware returns plain-text 401 `Invalid or missing auth token`; axum extractor rejections are plain text (400, 413, 415, 422). The client branches on `Content-Type` before parsing.

#### 4.2.2 Capabilities (NEW, M2a): `GET /api/chat/capabilities`

Protected, not rate-limited. A 404 means a daemon without M2a; the client then uses legacy mode (3.7).

```json
{
  "api_version": 1,
  "server_version": "2.1.9",
  "engine": "v2",
  "features": ["status", "attachments", "transcribe", "stream"],
  "limits": {
    "max_content_bytes": 100000,
    "max_attachments": 5,
    "max_attachment_bytes": 5242880,
    "max_total_attachment_bytes": 7340032,
    "attachment_mime_types": ["image/png", "image/jpeg", "image/gif", "image/webp"],
    "max_transcribe_b64_bytes": 25165824,
    "transcribe_formats": ["wav", "ogg", "opus", "mp3", "mp4", "webm", "flac", "m4a", "aac"],
    "messages_per_minute": 30,
    "transcribes_per_minute": 10,
    "max_request_bytes": 10485760
  },
  "voice": {"provider": "lunarvoice", "stt": true, "tts": true},
  "credential": "client"
}
```

- `engine`: `"v2"` when `crate::bridge::is_engine_v2_enabled()` (`ic/src/bridge/router.rs:48-52`), else `"v1"`.
- `features`: `status` and `attachments` always; `transcribe` when `state.voice.is_some()`; `stream` when `engine == "v2"`; canonical order.
- `voice.provider`: `"lunarvoice"` when `VoiceApi` exists, else `null` (and `stt`/`tts` false).
- `credential`: `"client"` when the request carried the `ClientCredential` extension, else `"master"`. M2a has no client tokens and always reports `"master"`; M2b adds `"client"`.
- The endpoint's existence implies correlation (4.1), lifecycle events, G5 image validation and V2 generated images.

`GET /api/voice/status` (exists, `voice.rs:432-452`) gains, additively: `"stt": true, "tts": true, "provider": "lunarvoice", "rate_limit": {"requests": 10, "window_secs": 60}`.

#### 4.2.3 Threads

- Create: `POST /api/chat/thread/new` (no body) → `{"id":"5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10","state":"Idle","turn_count":0,"created_at":"…","updated_at":"…","thread_type":"thread","channel":"gateway"}`.
- List: `GET /api/chat/threads` → `{"assistant_thread":{…},"threads":[{…,"title":"…"}],"active_thread":null}`. Ignore `active_thread` (legacy V1 pointer).
- No server-side select. Always use UUID thread ids from these endpoints: V2 adopts the client's UUID as the engine thread id (`router.rs:3952-3975`), so SSE `thread_id` equals the UI thread id; a non-UUID id makes V2 mint its own and history returns 400.

#### 4.2.4 `POST /api/chat/send` (`server.rs:1375-1458`)

```json
{
  "content": "what is in this screenshot?",
  "thread_id": "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10",
  "timezone": "Europe/Berlin",
  "images": [{"media_type": "image/png", "data": "iVBORw0KGgoAAAANSUhEUgAA..."}],
  "client_turn_id": "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e"
}
```

- `content` required (may be `""` with images); `thread_id` required (400 `thread_id is required for /api/chat/send`); `timezone` optional (`X-Timezone` fallback).
- `images[]`: base64 without a `data:` prefix. After M2a (G5): at most 5, each ≤ 5 MiB decoded, ≤ 7 MiB in total, type sniffed from magic bytes and one of PNG/JPEG/GIF/WebP; a rejection fails the whole request with **413** (size) or **422** (count, type, base64) and a plain-text reason naming the image index, for example `image 1 rejected: unsupported image type (allowed: png, jpeg, gif, webp)`. Today invalid items are silently skipped and SVG is accepted (`server.rs:1318-1365`).
- `client_turn_id` (NEW): optional, `is_valid_client_id`, else 400 `invalid client_turn_id`.
- Response: **202** `{"message_id":"0b8a6c1e-…","status":"accepted"}` (unchanged).
- Errors: 400; 429 `Rate limit exceeded. Try again shortly.` (30 per 60 s per user, shared with the web UI); 503 `Channel not started`; 500 `Channel closed`; 413/415/422 from axum or G5.
- The daemon builds `IncomingMessage::new("gateway", user_id, content).with_thread(thread_id)` with metadata (NEW fields marked):

```json
{"user_id": "derpy", "thread_id": "5f0e4a52-…", "message_id": "0b8a6c1e-…", "client_turn_id": "0193f2a1-…"}
```

- Scheduling: V2 user input runs in parallel lanes keyed by `(channel, user, thread)`, at most 4 (`ic/src/agent/dispatch.rs:7`, `:26-34`); same-thread messages run FIFO; controls and approvals are exclusive.

#### 4.2.5 Event stream `GET /api/chat/events` (`server.rs:1888-1900`, `sse.rs`)

- `text/event-stream`; each frame is `event: <type>` + one `data: <json>` line carrying the same `type` tag (`sse.rs:194-234`). Keep-alive: an empty comment every 15 s (`sse.rs:245`). Client watchdog: reconnect after 45 s without bytes.
- Scope: events for the authenticated `user_id`, plus global events (`user_id=None`). The client filters by `thread_id`. One connection per tenant. Cap: 100 SSE+WS connections (`sse.rs:19`), beyond it **503** `Too many connections`.
- Lag: broadcast buffer 256 events (`sse.rs:47-48`); a lagging subscriber receives `stream_gap {missed_events}` and continues. No `id:` and no `Last-Event-ID`; recovery is by resync (3.8).
- Size: `image_generated` carries a full data URL; parsers accept lines of at least 24 MiB and decode UTF-8 in streaming mode.
- Correlation (NEW, M2a): events caused by a gateway message carry top-level `message_id` and, when the client sent one, `client_turn_id`.

Event catalog (Corr = carries `message_id`/`client_turn_id` after M2a):

| `type` | Fields | Producer | Corr |
|---|---|---|---|
| **`turn_started`** (NEW) | `thread_id` | lifecycle `Started` (8.2), messages with G1 metadata only | yes |
| **`turn_finished`** (NEW) | `thread_id`, `outcome` (4.1) | lifecycle `Finished`, after any `response` for the message | yes |
| `response` | `content`, `thread_id`, `emotes?` | `GatewayChannel::respond` (`mod.rs:418-446`); also proactive `broadcast` (`mod.rs:590-615`) and mission results | yes for `respond`, no for proactive |
| `thinking` | `message`, `thread_id?` | V1 and V2 status text ("Processing...", "Calling LLM...", "Step complete — N in / M out tokens") | yes |
| `stream_chunk` | `content`, `thread_id?` | V2 only (`router.rs:4585-4593`); `response.content` is canonical | yes |
| `tool_started` / `tool_result` / `tool_completed` | `name` / `name, preview` / `name, success, error?, parameters?` | V1 and V2 (V2 preview ≤ 1000 bytes; V2 `parameters` is the duration) | yes |
| `reasoning_update` | `narrative`, `decisions[{tool_name, rationale}]` | V1, V2 | yes |
| `status` | `message`, `thread_id?` | misc; an empty `message` must be ignored | yes |
| `approval_needed` | `request_id`, `tool_name`, `description`, `parameters` (redacted pretty JSON), `thread_id?`, `allow_always` | V1, V2 (`mod.rs:504-518`, `router.rs:370-385`) | yes |
| `gate_required` | `request_id`, `gate_name`, `tool_name`, `description`, `parameters`, `resume_kind`, `thread_id?` | V2 direct SSE (`router.rs:346-363`); dedupe with `approval_needed` by `request_id` | yes |
| `gate_resolved` | `request_id`, `gate_name`, `tool_name`, `resolution`, `message`, `thread_id?` | V2 | optional |
| `auth_required` / `auth_completed` | `extension_name`, `instructions?`, `auth_url?`, `setup_url?` / `extension_name`, `success`, `message` | V1, V2, auth handlers | via `send_status` only |
| `image_generated` | `data_url`, `path?`, `thread_id?` | V1 (`dispatcher.rs:930-962`); V2 after G3 (8.3.6) | yes |
| `turn_cost`, `suggestions` | as today | V1 only | yes |
| `thread_state_changed`, `turn_recovery`, `child_thread_spawned` | as today | V2 | yes |
| `job_*`, `extension_status`, `heartbeat`, `error`, `stream_gap` | as today | job bus, extensions, global, unused, lag | no |

Typical V2 turn after M2a (one image tool call):

```
event: turn_started          data: {"type":"turn_started","thread_id":"T","message_id":"M","client_turn_id":"C"}
event: thinking              data: {"type":"thinking","message":"Processing...","thread_id":"T","message_id":"M","client_turn_id":"C"}
event: thread_state_changed  data: {"type":"thread_state_changed","thread_id":"T","from_state":"Created","to_state":"Running",…corr}
event: stream_chunk          data: {"type":"stream_chunk","content":"Let me draw","thread_id":"T",…corr}
event: tool_started          data: {"type":"tool_started","name":"image_generate","thread_id":"T",…corr}
event: image_generated       data: {"type":"image_generated","data_url":"data:image/png;base64,iVBOR…","thread_id":"T",…corr}
event: tool_completed        data: {"type":"tool_completed","name":"image_generate","success":true,"parameters":"8123ms","thread_id":"T",…corr}
event: response              data: {"type":"response","content":"Here is the image.","thread_id":"T",…corr}
event: turn_finished         data: {"type":"turn_finished","thread_id":"T","outcome":"replied","message_id":"M","client_turn_id":"C"}
```

#### 4.2.6 Correlation implementation outline (M2a, details in 8.3.1)

`chat_send_handler` and `chat_approval_handler` stamp the metadata; `SseManager` carries an optional `Correlation{message_id, client_turn_id}` per broadcast and merges both fields into the event JSON at serialization; `GatewayChannel::send_status`, `respond` and `message_lifecycle` read it from the message metadata; the V2 bridge already clones the original metadata into every forwarded status (`engine_status_metadata`, `router.rs:4106-4120`) and uses the correlated broadcast for its direct SSE events.

#### 4.2.7 Approvals and gates

- Prompts: `approval_needed` (always) and `gate_required` (V2). Pending list after reconnect: `GET /api/chat/pending-gates` → `{"gates":[{"request_id","thread_id","gate_name","tool_name","description","parameters","resume_kind"}]}` (`server.rs:1460-1476`), plus `history.pending_approval`.
- Decide: `POST /api/chat/approval` (`server.rs:1478-1558`):

```json
{"request_id": "6c0f2a8e-1f9b-4d3e-a1c2-7b5e9d0f4a21", "action": "approve",
 "thread_id": "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10", "client_turn_id": "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e"}
```

  `action` is `approve`, `always` or `deny`. `client_turn_id` (NEW) is the **owning turn's** id. 202 `{"message_id","status":"accepted"}`. Errors (plain text): 400 `Unknown action: x`, `Invalid request_id (expected UUID)`, `thread_id must not be blank when supplied`, `thread_id does not own this approval request`; **404** `No pending approval matches this request_id`; 503 when gate inspection fails. Not rate-limited. NEW: the handler stamps the same metadata as `send` (today it sets none, so resumed-turn events are broadcast globally).
- Credentials: `POST /api/chat/auth-token {"extension_name","token","request_id?","thread_id?","client_turn_id?"}` (both ids together or neither) and `POST /api/chat/auth-cancel {"extension_name","request_id?","thread_id?","client_turn_id?"}` (`server.rs:1625-1720`, `:1836-1869`).
  - NEW `client_turn_id` (M2a, validated like `send`): the V2 resume message built by `queue_engine_auth_resume` (`server.rs:1780-1834`) carries the gateway metadata with it, so the resumed turn's events and its `turn_started`/`turn_finished` belong to the owning turn (F1). Today that message has no `message_id` and no client id (`server.rs:1799-1804`).
  - Auth-cancel runs outside the agent loop (`server.rs:1857-1864`) and emits no lifecycle; on V2 the cancelled gate is announced by `gate_resolved{resolution:"cancelled"}` (`ic/src/bridge/router/auth.rs:102-121`), and the client seals the owning turn when the POST returns 2xx (3.6.4).
  - With a client token (M2b) `auth-token` requires a pending authentication request for the extension: a V2 gate matched by `requested_engine_auth_gate` (`server.rs:1739-1777`), or a V1 thread of the owner's session whose `pending_auth` names the extension and is not expired (`ic/src/agent/session.rs:149-162`). The master token keeps today's behaviour, which configures any installed extension and may overwrite existing secrets (`ic/src/extensions/manager.rs:6094-6176`).
- Authority: approvals resolve against the authenticated owner's gates. The TUI holds the same approval authority as the web UI.

#### 4.2.8 Interrupt and steer

No cancel endpoint for chat turns. Interrupt: `POST /api/chat/send {"content":"/interrupt","thread_id":"T","client_turn_id":"<new id>"}` (`/stop` is equivalent). It is a priority control handled at once; V2 stops every running thread in that conversation (`router.rs:3192-3285`). The control gets `response` "Interrupted." or "Nothing to interrupt." and `turn_finished{replied}` under its own id; the interrupted turn gets `turn_finished{interrupted}` (V1 also sends it a `response` "Interrupted."). Queued same-lane prompts removed by the interrupt get `turn_finished{cancelled}`. Steer: `/steer <note>` (`ic/src/agent/submission.rs:29-38`); its acknowledgement is a `response` under the steer's id. Both count against the 30/min limit.

#### 4.2.9 History

`GET /api/chat/history?thread_id=<uuid>&limit=50&before=<rfc3339>` (`server.rs:1956-2152`):

```json
{"thread_id":"5f0e…","turns":[{"turn_number":0,"user_input":"hi","response":"hello","state":"Completed",
  "started_at":"2026-11-03T00:10:02Z","completed_at":"2026-11-03T00:10:05Z",
  "tool_calls":[{"name":"http","has_result":true,"has_error":false,"result_preview":"…"}],
  "reasoning":{"narrative":"…","decisions":[]}}],
 "has_more":false,"oldest_timestamp":"…",
 "pending_approval":{"request_id":"…","tool_name":"shell","description":"…","parameters":"{…}"}}
```

Gotchas: durable V2 history shows an in-flight turn as `Failed`; `user_input` is the effective content (with an `<attachment>` block for images); history carries no message ids.

#### 4.2.10 Voice

- Available only when `TRANSCRIPTION_PROVIDER=lunarvoice` and `VOICE_AUTH_TOKEN` are set, independent of `TRANSCRIPTION_ENABLED` (`voice.rs:240-283`; built once at startup, `ic/src/main.rs:816-821`). Without it, status reports `configured:false` and POSTs answer 503 `{"error":"unavailable","message":"voice service is not configured"}`.
- `GET /api/voice/status` → `{"configured":true,"model":"…","max_audio_b64_len":25165824,"max_text_chars":10000,"max_synth_audio_secs":300}` (+ M2a fields above).
- `POST /api/voice/transcribe` (32 MiB route body limit): `{"audio":"<b64>","format":"wav"}` → `{"text":"turn on the hallway lights","language":"en","confidence":0.93,"segments":[…],"model":"parakeet-tdt-0.6b-v2-int8"}`. Omit `language`.
- `POST /api/voice/synthesize` exists (TTS); unused in this design.
- Error matrix (JSON unless noted; `voice.rs:61-91`, `:550-616`):

| HTTP | `error` | Meaning |
|---|---|---|
| 400 | `bad_request` | validation, or the sidecar rejected the model or language |
| 401 | (plain text) | bearer invalid |
| 413 | `payload_too_large` | base64 over 24 MiB (plain text if the 32 MiB route limit trips first) |
| 422 | `processing_failed` | sidecar could not decode or process |
| 429 | `rate_limited` | 10 per 60 s per user (STT and TTS share it), or the sidecar's limiter |
| 500 / 502 | `upstream_error` | sidecar rejected the gateway credential / unreachable or malformed |
| 503 | `unavailable` | not configured (`voice service is not configured`) or sidecar busy (`voice sidecar is unavailable`, one admitted call per component) |
| 504 | `upstream_timeout` | sidecar or proxy timeout |

#### 4.2.11 Inputs the TUI never sends as chat

`/quit`, `/exit`, `/shutdown` (3.10.2; refused server-side after M2a, 8.4); `/restart` (refused locally, R17); bare approval keywords without a pending prompt unless confirmed (3.10.3).

#### 4.2.12 TypeScript wire types (`tui/src/transport/gateway-wire.ts`)

```ts
export interface WireLimits { max_content_bytes: number; max_attachments: number; max_attachment_bytes: number;
  max_total_attachment_bytes: number; attachment_mime_types: string[]; max_transcribe_b64_bytes: number;
  transcribe_formats: string[]; messages_per_minute: number; transcribes_per_minute: number;
  max_request_bytes?: number; max_frame_bytes?: number; max_in_flight?: number; idle_timeout_secs?: number }
export interface ChatCapabilities { api_version: number; server_version: string; engine: "v1" | "v2";
  features: string[]; limits: WireLimits; voice: { provider: string | null; stt: boolean; tts: boolean };
  credential: "master" | "client" }
export interface ImageData { media_type: "image/png" | "image/jpeg" | "image/gif" | "image/webp"; data: string }
export interface SendMessageRequest { content: string; thread_id: string; timezone?: string; images?: ImageData[]; client_turn_id?: string }
export interface SendMessageResponse { message_id: string; status: "accepted" }
export interface ApprovalRequestBody { request_id: string; action: "approve" | "always" | "deny"; thread_id?: string; client_turn_id?: string }
export interface AuthTokenRequestBody { extension_name: string; token: string; request_id?: string; thread_id?: string; client_turn_id?: string }
export interface AuthCancelRequestBody { extension_name: string; request_id?: string; thread_id?: string; client_turn_id?: string }
export type ResumeKind = { Approval: { allow_always: boolean } }
  | { Authentication: { credential_name: string; instructions: string; auth_url: string | null } }
  | { External: { callback_id: string } }
type Corr = { message_id?: string; client_turn_id?: string }
export type GatewayEvent = Corr & (
  | { type: "turn_started"; thread_id: string }
  | { type: "turn_finished"; thread_id: string; outcome: string }
  | { type: "response"; content: string; thread_id: string; emotes?: string[] }
  | { type: "thinking"; message: string; thread_id?: string }
  | { type: "stream_chunk"; content: string; thread_id?: string }
  | { type: "stream_gap"; missed_events: number }
  | { type: "tool_started"; name: string; thread_id?: string }
  | { type: "tool_result"; name: string; preview: string; thread_id?: string }
  | { type: "tool_completed"; name: string; success: boolean; error?: string; parameters?: string; thread_id?: string }
  | { type: "reasoning_update"; narrative: string; decisions: { tool_name: string; rationale: string }[]; thread_id?: string }
  | { type: "status"; message: string; thread_id?: string }
  | { type: "approval_needed"; request_id: string; tool_name: string; description: string; parameters: string; thread_id?: string; allow_always: boolean }
  | { type: "gate_required"; request_id: string; gate_name: string; tool_name: string; description: string; parameters: string; resume_kind: ResumeKind; thread_id?: string }
  | { type: "gate_resolved"; request_id: string; gate_name: string; tool_name: string; resolution: string; message: string; thread_id?: string }
  | { type: "auth_required"; extension_name: string; instructions?: string; auth_url?: string; setup_url?: string }
  | { type: "auth_completed"; extension_name: string; success: boolean; message: string }
  | { type: "image_generated"; data_url: string; path?: string; thread_id?: string }
  | { type: "turn_cost"; input_tokens: number; output_tokens: number; cost_usd: string; thread_id?: string }
  | { type: "suggestions"; suggestions: string[]; thread_id?: string }
  | { type: "skill_activated"; skill_names: string[]; thread_id?: string }
  | { type: "thread_state_changed"; thread_id: string; from_state: string; to_state: string; reason?: string }
  | { type: "turn_recovery"; thread_id: string; failure_kind: string; recovery_state: string; recovery_attempt: number; message: string }
  | { type: "job_started"; job_id: string; title: string; browse_url: string }
  | { type: "job_status"; job_id: string; message: string }
  | { type: "job_result"; job_id: string; status: string; session_id?: string }
  | { type: "error"; message: string; thread_id?: string }
  | { type: "heartbeat" })
```

History, thread, pending-gate and voice DTOs are as listed in `design/area-gateway.md` 1.12 (unchanged server types).

### 4.3 REPLv3 wire specification

REPLv3 is **protocol 3** on the Unix socket. **Protocol 1** is today's line protocol, kept byte for byte for `lunarwing repl` (`ic/src/cli/repl.rs`) and `ic/unix-socket-client`. Protocol 2 is never used (vibetui4's "protocol v2" never shipped on master). Rust types live in NEW `ic/crates/lunarwing_common/src/repl_wire.rs` (M3), used by the server; a hand-written TypeScript mirror in `tui/src/transport/replv3-wire.ts` (M4) is checked against golden fixtures in `ic/crates/lunarwing_common/tests/fixtures/replv3/`.

#### 4.3.1 Framing

- One UTF-8 JSON value per line, `\n`-terminated; a trailing `\r` is tolerated; empty lines are ignored.
- Before the handshake: line cap `MAX_HANDSHAKE_BYTES = 16384`; the first line must be `Connect` within `HANDSHAKE_TIMEOUT = 10 s`.
- After the handshake: line cap `max_frame_bytes = 16842752` (16 MiB + 64 KiB) in both directions and both protocols. A longer line gets `Error{code:"too_large", fatal:true}` (v3) and the connection closes; no resynchronization.
- Unknown JSON fields are ignored (serde default; no `deny_unknown_fields`). Unknown variants: v3 answers `Error{bad_frame}` and continues; v1 logs the frame kind and continues (today's behaviour).

#### 4.3.2 Envelope and types

serde externally tagged: `{"<Variant>":{...}}`; unit variants are bare strings (`"Ping"`, `"Pong"`). Each variant wraps a named struct. v3-only optional fields use `#[serde(default, skip_serializing_if = …)]`; the v1 fields `client_info` and `session_id` keep serializing `null` so v1 encodings stay byte-identical.

```rust
// ic/crates/lunarwing_common/src/repl_wire.rs (NEW, M3)
pub const PROTOCOL_LEGACY: u32 = 1;
pub const PROTOCOL_V3: u32 = 3;
pub const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024 + 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub enum ClientFrame {
    Connect(ConnectFrame), Message(MessageFrame), Interrupt(InterruptFrame), Steer(SteerFrame),
    Approval(ApprovalFrame), Transcribe(TranscribeFrame), Disconnect(DisconnectFrame), Ping, Pong,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum ServerFrame {
    Welcome(WelcomeFrame), Ack(AckFrame), TurnStarted(TurnFrame), TurnFinished(TurnFinishedFrame),
    Status(StatusFrame), Response(ResponseFrame), ApprovalRequest(ApprovalRequestFrame),
    ApprovalResolved(ApprovalResolvedFrame), AuthRequired(AuthRequiredFrame), Error(ErrorFrame),
    TranscribeResult(TranscribeResultFrame), Notice(NoticeFrame), Ping, Pong,
}
impl ClientFrame { pub fn kind(&self) -> &'static str }   // "Connect", "Message", ...
impl ServerFrame { pub fn kind(&self) -> &'static str }
pub fn encode_line(frame: &ServerFrame) -> serde_json::Result<Vec<u8>>;          // JSON + b'\n'
pub fn encode_client_line(frame: &ClientFrame) -> serde_json::Result<Vec<u8>>;

pub struct ConnectFrame {
    pub version: String,
    pub client_info: Option<String>,                                   // v1: serializes null
    #[serde(default, skip_serializing_if = "Option::is_none")] pub protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]   pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub thread: Option<String>,
}
pub struct MessageFrame {
    pub content: String,
    pub session_id: Option<String>,                                    // v1 field; ignored; serializes null
    #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]   pub attachments: Vec<WireAttachment>,
}
pub struct WireAttachment { pub mime_type: String, #[serde(default)] pub filename: Option<String>, pub data_base64: String }
pub struct InterruptFrame { #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String> }
pub struct SteerFrame { #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>, pub content: String }
pub struct ApprovalFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>,
    pub request_id: String,                                            // UUID text
    pub decision: ApprovalDecision,
}
#[serde(rename_all = "snake_case")] pub enum ApprovalDecision { Approve, Always, Deny }
pub struct TranscribeFrame { pub req_id: String, #[serde(default = "default_audio_format")] pub format: String, pub audio: String }
pub struct DisconnectFrame { pub session_id: Option<String>, pub reason: Option<String> }

pub struct WelcomeFrame {
    pub protocol: u32, pub server_version: String, pub agent_name: String,
    pub engine: String,                                                // "v1" | "v2": how the channel is routed now
    pub session_id: String, pub thread: String,
    pub features: Vec<String>,                                         // accepted optional features, canonical order
    pub limits: crate::client_caps::WireLimits,
}
pub struct AckFrame { pub msg_id: String }
pub struct TurnFrame { pub msg_id: String }
pub struct TurnFinishedFrame { pub msg_id: String, pub outcome: crate::client_caps::TurnOutcome, #[serde(default)] pub dropped_status: u64 }
pub struct StatusFrame { #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>, pub event: serde_json::Value }
pub struct ResponseFrame {
    pub content: String, pub session_id: Option<String>,
    pub is_complete: bool,                                             // always true; partial text only via stream_chunk
    #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>,
}
pub struct ApprovalRequestFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>,
    pub request_id: String, pub tool_name: String, pub description: String,
    pub parameters: String,                                            // redacted pretty JSON, untruncated (gateway shape)
    pub allow_always: bool,
    #[serde(default)] pub parameters_truncated: bool,                  // true only if the frame would exceed max_frame_bytes (4.3.6)
    #[serde(default)] pub parameters_bytes: u64,                       // byte length of the full parameters text
}
pub struct ApprovalResolvedFrame { pub request_id: String, pub resolution: ApprovalResolution }
#[serde(rename_all = "snake_case")] pub enum ApprovalResolution { Approve, Always, Deny, Cancelled, #[serde(other)] Unknown }
pub struct AuthRequiredFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>,
    pub extension_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub auth_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub setup_url: Option<String>,
}
pub struct ErrorFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub req_id: Option<String>,
    pub code: ErrorCode, pub message: String, pub retryable: bool, pub fatal: bool,
}
pub struct TranscribeResultFrame {
    pub req_id: String, pub text: String, pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub model: Option<String>,
}
pub struct NoticeFrame {
    pub kind: NoticeKind, pub level: NoticeLevel, pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub msg_id: Option<String>,
}
#[serde(rename_all = "snake_case")] pub enum NoticeKind { Background, Broadcast, Shutdown, #[serde(other)] Unknown }
#[serde(rename_all = "snake_case")] pub enum NoticeLevel { Info, Warning, #[serde(other)] Unknown }
#[serde(rename_all = "snake_case")] pub enum ErrorCode {
    BadFrame, TooLarge, FeatureNotNegotiated, UnsupportedMedia, RateLimited, Busy, ServerBusy, ThreadInUse,
    NoPendingApproval, UnknownRequest, ForbiddenCommand, Unavailable, Timeout, NoSpeech,
    TranscribeFailed, IdleTimeout, SlowConsumer, Shutdown, Internal, #[serde(other)] Unknown,
}
```

All structs derive `Clone, Serialize, Deserialize` (with `PartialEq` for tests). `Debug` is hand-written for every struct that carries user content and prints `<redacted N bytes>` for `MessageFrame.content`, `WireAttachment.data_base64`, `SteerFrame.content`, `TranscribeFrame.audio`, `ResponseFrame.content`, `TranscribeResultFrame.text`, `NoticeFrame.text`, `ApprovalRequestFrame.parameters` and `StatusFrame.event` (type only), through a helper `pub struct Redacted(pub usize)`. `#[serde(other)]` on the last unit variant of an externally tagged enum passes serde_derive's checks (`ic/vendor/serde_derive/src/internals/check.rs:144-188`).

#### 4.3.3 Negotiation

| `Connect.protocol` | Served as | First server frame |
|---|---|---|
| absent, 0, 1, 2 | protocol 1 (includes vibetui4-era clients that send `features` without `protocol`) | v1 `Response{"content":"Connected to <agent> REPL. Session: <id>","session_id":"<id>","is_complete":true}` (today's text, `ic/src/channels/unix_socket_repl.rs:181-186`) |
| ≥ 3 | protocol 3 (a higher value is answered with 3) | `Welcome` |

- Legacy daemon detection: a v3 `Connect` parses on today's daemon (unknown fields ignored, `unix_socket_repl.rs:34-61`) and is answered with the v1 welcome `Response`; a `Response` as the first frame therefore means "legacy daemon" (fixes D10 on the client side).
- In protocol 1, `features` and `thread` are ignored; a `Message` with non-empty `attachments` is rejected with `Response{"content":"Error: attachments require REPLv3 (protocol 3)","is_complete":true}` and not forwarded; v3-only frames get `Response{"Error: <Frame> requires REPLv3 (protocol 3)"}`.
- Core in protocol 3 (never negotiated): `Ack`, `TurnStarted`/`TurnFinished`, `Response` with `msg_id`, `ApprovalRequest`/`ApprovalResolved`, `AuthRequired`, `Error`, `Notice`, `Ping`/`Pong`, per-connection threads, thread resume.
- Optional features:

| Feature | Accepted when | Effect |
|---|---|---|
| `status` | requested | `Status` frames for this connection's messages |
| `attachments` | requested | `Message.attachments` (images) accepted |
| `transcribe` | requested and the daemon has a `VoiceApi` (LunarVoice configured) | `Transcribe` accepted; `limits.max_transcribe_b64_bytes > 0` |
| `stream` | requested, `status` accepted, and `crate::bridge::should_route_to_engine_v2("unix_socket_repl")` | `stream_chunk` status events sent; always false today |

- `Welcome.features` lists only accepted optional features in canonical order. Unknown requested names are ignored. A frame or field of a feature that was not accepted gets `Error{feature_not_negotiated}`.
- Thread token: `Connect.thread` must pass `is_valid_client_id`, else `Error{bad_frame, fatal:true}`; absent → the session id. The token in effect is echoed in `Welcome.thread`; the connection's conversation scope is `unix:<token>` (8.6.2). A token bound to another live v3 connection is refused with `Error{thread_in_use, retryable:true, fatal:true}` (F3); once that connection has closed the token can be reused (reconnect, `/thread resume`).

#### 4.3.4 Client frames

Ids: `msg_id` and `req_id` pass `is_valid_client_id`; invalid → `Error{bad_frame}` without id; a `msg_id` still in flight on this thread → `Error{bad_frame, message:"duplicate msg_id"}`; absent `msg_id` on a v3 `Message`/`Interrupt`/`Steer`/`Approval` → the server assigns `s-<uuid32>` and returns it in `Ack`.

| Frame | Server action | Replies |
|---|---|---|
| `Connect` | handshake | `Welcome` or the v1 welcome `Response` |
| `Message{content <= 100000 bytes, msg_id, attachments}` | validate, forward | `Ack`, then `TurnStarted`, `Status`*, `ApprovalRequest`/`AuthRequired`*, `Response`*, `TurnFinished`; or a terminal `Error` |
| `Interrupt{msg_id}` | forward `/interrupt` on this thread | `Ack`, `TurnStarted`, `Response("Interrupted.")` or `Response("Nothing to interrupt.")`, `TurnFinished` under the interrupt's id |
| `Steer{msg_id, content}` | forward `/steer <content>` | `Ack`, `TurnStarted`, `Response("Steer note queued …")`, `TurnFinished` |
| `Approval{msg_id, request_id, decision}` | check pending on this thread, forward `Submission::ExecApproval` JSON | `Ack`, `ApprovalResolved`, continuation under the original `msg_id`; or `Error{unknown_request}` |
| `Transcribe{req_id, format, audio}` | spawned LunarVoice call (8.6.3) | `TranscribeResult` or `Error{req_id}` |
| `Disconnect` | close | none |
| `"Ping"` / `"Pong"` | answer / ignore | `"Pong"` |

```json
{"Connect":{"version":"wingtip/0.2.0","client_info":"opentui/0.8.1 bun/1.3.14","protocol":3,"features":["status","attachments","transcribe"],"thread":"tui-derpy-7f3c2a9e41d0b6c5"}}
{"Connect":{"version":"2.1.8","client_info":null}}
{"Message":{"content":"What is in this screenshot?","session_id":null,"msg_id":"c-0007","attachments":[{"mime_type":"image/png","filename":"shot.png","data_base64":"iVBORw0KGgo="}]}}
{"Message":{"content":"hello","session_id":null}}
{"Interrupt":{"msg_id":"c-0008"}}
{"Steer":{"msg_id":"c-0009","content":"use the staging host instead"}}
{"Approval":{"msg_id":"c-0010","request_id":"5f0c1d9e-2b7a-4c7e-9a51-0d3f8e6b1a22","decision":"approve"}}
{"Transcribe":{"req_id":"t-0003","format":"wav","audio":"UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA="}}
{"Disconnect":{"session_id":null,"reason":"client requested disconnect"}}
"Ping"
"Pong"
```

The second `Connect` and second `Message` are the exact v1 encodings written by `ic/src/cli/repl.rs:80-87`, `:133-140` and `ic/unix-socket-client/src/lib.rs:62-70`. The `Transcribe` audio is a valid 44-byte WAV header (16 kHz mono s16, zero samples).

#### 4.3.5 Server frames

| Frame | When | Outbound class |
|---|---|---|
| `Welcome` | after a valid v3 `Connect` | control |
| `Ack{msg_id}` | validated and handed to the agent queue; not "started" (the scheduler may defer it) | control |
| `TurnStarted{msg_id}` | lifecycle `Started`; again for an approval continuation of the original id | control |
| `Status{msg_id?, event}` | progress for this connection's messages (feature `status`) | status; `image_generated` is control |
| `Response{content, session_id, is_complete:true, msg_id?}` | a reply; rarely more than one per `msg_id` (V1 drain loop); `TurnFinished` is the authority | control |
| `TurnFinished{msg_id, outcome, dropped_status}` | lifecycle `Finished`, after any reply | control |
| `ApprovalRequest{…}` | a tool on this thread needs approval | control |
| `ApprovalResolved{request_id, resolution}` | a decision was forwarded, or `/interrupt`, `/clear`, `/new`, `/undo` on the thread cancelled it; sent to every v3 connection on the thread | control |
| `AuthRequired{…}` | an extension needs a credential; the next `Message` on the thread is the credential | control |
| `Error{…}` | a frame was rejected, or the connection is closing | control |
| `TranscribeResult{…}` | transcription done | control |
| `Notice{kind, level, text, msg_id?}` | unsolicited text: job monitor, broadcast, shutdown | control |
| `"Pong"` | reply to `"Ping"` | control |

`Error` with `msg_id` or `req_id` is terminal for that id. `fatal:true` means the server closes after sending it.

```json
{"Welcome":{"protocol":3,"server_version":"2.1.9","agent_name":"LunarWing","engine":"v1","session_id":"0b6c1c2d3e4f4a5b8c9d0e1f2a3b4c5d","thread":"tui-derpy-7f3c2a9e41d0b6c5","features":["status","attachments","transcribe"],"limits":{"max_content_bytes":100000,"max_attachments":5,"max_attachment_bytes":5242880,"max_total_attachment_bytes":10485760,"attachment_mime_types":["image/png","image/jpeg","image/gif","image/webp"],"max_transcribe_b64_bytes":16777216,"transcribe_formats":["wav","ogg","opus","mp3","mp4","webm","flac","m4a","aac"],"messages_per_minute":60,"transcribes_per_minute":10,"max_frame_bytes":16842752,"max_in_flight":8,"idle_timeout_secs":180}}}
{"Ack":{"msg_id":"c-0007"}}
{"TurnStarted":{"msg_id":"c-0007"}}
{"Status":{"msg_id":"c-0007","event":{"type":"thinking","message":"Processing..."}}}
{"Status":{"msg_id":"c-0007","event":{"type":"tool_completed","name":"shell","success":false,"error":"exit status 1","parameters":"{\n  \"command\": \"[REDACTED]\"\n}"}}}
{"Status":{"msg_id":"c-0007","event":{"type":"image_generated","data_url":"data:image/png;base64,iVBORw0KGgo=","path":"images/cat.png"}}}
{"Status":{"event":{"type":"stream_gap","missed_events":12}}}
{"Response":{"content":"It is a terminal screenshot showing a failing test run.","session_id":"0b6c1c2d3e4f4a5b8c9d0e1f2a3b4c5d","is_complete":true,"msg_id":"c-0007"}}
{"TurnFinished":{"msg_id":"c-0007","outcome":"replied","dropped_status":0}}
{"ApprovalRequest":{"msg_id":"c-0011","request_id":"5f0c1d9e-2b7a-4c7e-9a51-0d3f8e6b1a22","tool_name":"shell","description":"Run shell command: rm -rf build/","parameters":"{\n  \"command\": \"rm -rf build/\"\n}","allow_always":false,"parameters_truncated":false,"parameters_bytes":32}}
{"TurnFinished":{"msg_id":"c-0011","outcome":"paused","dropped_status":0}}
{"ApprovalResolved":{"request_id":"5f0c1d9e-2b7a-4c7e-9a51-0d3f8e6b1a22","resolution":"approve"}}
{"AuthRequired":{"msg_id":"c-0012","extension_name":"github","instructions":"Paste a GitHub personal access token as your next message.","setup_url":"https://github.com/settings/tokens"}}
{"Error":{"msg_id":"c-0013","code":"unsupported_media","message":"image 0: unsupported image type (allowed: png, jpeg, gif, webp)","retryable":false,"fatal":false}}
{"Error":{"code":"too_large","message":"frame exceeds 16842752 bytes","retryable":false,"fatal":true}}
{"Error":{"code":"thread_in_use","message":"thread tui-derpy-7f3c2a9e41d0b6c5 is in use by another connection","retryable":true,"fatal":true}}
{"TranscribeResult":{"req_id":"t-0003","text":"remind me to rotate the backup keys tomorrow","elapsed_ms":1840,"language":"en","model":"parakeet-tdt-0.6b-v2-int8"}}
{"Notice":{"kind":"background","level":"info","text":"[Job 6e63414c] Container finished (status: completed)"}}
{"Notice":{"kind":"shutdown","level":"warning","text":"LunarWing is shutting down; reconnect later."}}
"Pong"
```

v1 server frames (unchanged):

```json
{"Response":{"content":"Connected to LunarWing REPL. Session: 0b6c1c2d3e4f4a5b8c9d0e1f2a3b4c5d","session_id":"0b6c1c2d3e4f4a5b8c9d0e1f2a3b4c5d","is_complete":true}}
{"Response":{"content":"Server busy: maximum connections (10) reached.","session_id":"0b6c1c2d3e4f4a5b8c9d0e1f2a3b4c5d","is_complete":true}}
```

#### 4.3.6 Status events are gateway SSE events

`Status.event` is `serde_json::to_value(status_sse_event(update, None))`, where NEW `pub(crate) fn status_sse_event(status: StatusUpdate, thread_id: Option<String>) -> SseEvent` is extracted from `GatewayChannel::send_status` (`ic/src/channels/web/mod.rs:448-584`) in M2a and called by both channels, so field names cannot drift.

| `StatusUpdate` (`ic/src/channels/channel.rs`) | `event` | REPLv3 handling |
|---|---|---|
| `Thinking`, `ToolStarted`, `ToolCompleted`, `ToolResult`, `Status`, `ExternalWaiting`, `SkillActivated`, `JobStarted`, `AuthCompleted`, `Suggestions`, `ReasoningUpdate`, `TurnCost` | gateway shape | status class; string fields truncated to 4 KiB with `lunarwing_common::truncate_preview` |
| `StreamChunk` | `stream_chunk` | only with feature `stream`, otherwise dropped (not counted) |
| `ImageGenerated{data_url, path}` | `image_generated` | control class; a `data_url` not starting with `data:image/` or over 16 MiB is replaced by `status{message:"Image generated: <path>"}` |
| `ApprovalNeeded{…}` | lifted to `ApprovalRequest` (`parameters = to_string_pretty(display parameters)`, never truncated; `parameters_bytes` is its length; only if the encoded frame would exceed `max_frame_bytes - 65536` are the parameters cut at a char boundary and `parameters_truncated` set, SEC-04) | core |
| `AuthRequired{…}` | lifted to `AuthRequired` | core |
| (server-generated) | `stream_gap{missed_events}` | sent before the next delivered status after shedding |

#### 4.3.7 Error codes and limits

| Code | Meaning | retryable | fatal |
|---|---|---|---|
| `bad_frame` | unparseable JSON, unknown variant, invalid id or token, duplicate `msg_id`, empty message, second `Connect`, invalid base64 | no | bad handshake only |
| `too_large` | line over the cap (fatal); content, attachment or audio over its limit | no | line only |
| `feature_not_negotiated` | frame or field of a feature not in `Welcome.features` | no | no |
| `unsupported_media` | image fails sniffing or the allowlist; audio format not in `transcribe_formats` | no | no |
| `rate_limited` | 60 agent frames/min (Message, Interrupt, Steer, Approval) or 10 transcriptions/min per connection; sidecar 429 | yes | no |
| `busy` | 8 messages or 20 MiB of attachments in flight (a turn paused at a gate releases its slot until its continuation starts, F2); a transcription already running on this connection; sidecar 503 | yes | no |
| `server_busy` | `max_connections` reached (v1 gets the legacy "Server busy" `Response`) | yes | yes |
| `thread_in_use` | `Connect.thread` is bound to another live v3 connection (F3) | yes | yes |
| `no_pending_approval` | approval keyword or `Approval` with nothing pending on this thread; stale approval | no | no |
| `unknown_request` | `request_id` not pending on this thread | no | no |
| `forbidden_command` | `/quit`, `/exit`, `/shutdown` | no | no |
| `unavailable` | no LunarVoice configured (retryable false; only if `Transcribe` is sent despite the feature check), or the sidecar is unreachable or rejected the daemon's credential (retryable true) | per frame | no |
| `timeout` | LunarVoice timeout (sidecar 504 or the 150 s server-side budget) | yes | no |
| `no_speech` | transcript empty after trimming | no | no |
| `transcribe_failed` | sidecar 400 or 422: the audio or request was rejected (detail logged server-side only) | no | no |
| `idle_timeout` | no frame within `idle_timeout_secs` (v3 only) | n/a | yes |
| `slow_consumer` | outbound queue overflow or write timeout | n/a | yes |
| `shutdown` | agent loop gone or daemon stopping | yes | yes |
| `internal` | serialization or invariant failure | no | no |

Server constants (overridable only through a `#[cfg(test)]` builder): handshake 16 KiB / 10 s / at most 8 pending handshakes; frame 16842752 bytes; content 100000 bytes; images 5 × 5 MiB, 10 MiB total; in flight 8 messages and 20 MiB of attachment bytes per connection; 60 messages and 10 transcriptions per minute per connection; idle timeout 180 s (v3 only; v1 has none because a lockstep client idles at its prompt); outbound queue 1024 frames / 32 MiB, status shed above 2 MiB queued, write timeout 15 s; transcription 16 MiB base64, one in flight per connection, 150 s.

#### 4.3.8 Sequences

A. Plain turn (V1 emits `Done`, `Suggestions`, `TurnCost` before the handler returns, so they precede the reply):

```
C {"Message":{"content":"hi","session_id":null,"msg_id":"c-1"}}
S {"Ack":{"msg_id":"c-1"}}
S {"TurnStarted":{"msg_id":"c-1"}}
S {"Status":{"msg_id":"c-1","event":{"type":"thinking","message":"Processing..."}}}
S {"Status":{"msg_id":"c-1","event":{"type":"status","message":"Done"}}}
S {"Status":{"msg_id":"c-1","event":{"type":"turn_cost","input_tokens":812,"output_tokens":41,"cost_usd":"$0.0004"}}}
S {"Response":{"content":"Hello!","session_id":"…","is_complete":true,"msg_id":"c-1"}}
S {"TurnFinished":{"msg_id":"c-1","outcome":"replied","dropped_status":0}}
```

B. Two messages back to back: each gets its own `Ack`, `TurnStarted`, `Response`, `TurnFinished` (C1 fixed at the protocol level). The gap between `Ack` and `TurnStarted` is "queued" (another channel may hold the global scheduler slot).

C. Approval:

```
C {"Message":{"content":"delete the build dir","session_id":null,"msg_id":"c-2"}}
S Ack c-2, TurnStarted c-2, Status …
S {"ApprovalRequest":{"msg_id":"c-2","request_id":"R","tool_name":"shell",…,"allow_always":false}}
S {"TurnFinished":{"msg_id":"c-2","outcome":"paused","dropped_status":0}}
C {"Approval":{"msg_id":"c-3","request_id":"R","decision":"approve"}}
S {"Ack":{"msg_id":"c-3"}}
S {"ApprovalResolved":{"request_id":"R","resolution":"approve"}}
S {"TurnStarted":{"msg_id":"c-2"}}
S Status c-2 (tool_started, tool_completed, …)
S {"Response":{…,"msg_id":"c-2"}}
S {"TurnFinished":{"msg_id":"c-2","outcome":"replied","dropped_status":0}}
```

D. Interrupt during a turn: `Interrupt{c-5}` gets `Ack c-5`, `TurnStarted c-5`, `Response{"Interrupted.", c-5}`, `TurnFinished{c-5, replied}`; the running turn `c-4` ends with `Response{"Interrupted.", c-4}` and `TurnFinished{c-4, interrupted}`; queued prompts of the thread removed by the interrupt get `TurnFinished{cancelled}`.

E. Bare keyword with nothing pending: `Message{"ok", c-6}` → `Error{msg_id:"c-6", code:"no_pending_approval"}`; nothing is forwarded.

F. Transcribe: `Transcribe{t-1}` → `TranscribeResult{t-1}` or `Error{req_id:"t-1"}`; message intake continues meanwhile.

G. Legacy v1 client and approval:

```
C {"Message":{"content":"delete the build dir","session_id":null}}
S {"Response":{"content":"Approval required for `shell`.\nRequest ID: `R`\nParameters:\n{…}\nReply with `yes`/`y` to approve or `no`/`n` to deny.","session_id":"…","is_complete":true}}
C {"Message":{"content":"yes","session_id":null}}
S {"Response":{"content":"<final reply>","session_id":"…","is_complete":true}}
```

The prompt wording mirrors the XMPP channel (`ic/src/channels/xmpp/mod.rs:1245-1265`). This removes today's permanent hang in `lunarwing repl`, which reads exactly one frame per send (`ic/src/cli/repl.rs:133-154`).

H. Reconnect with the same thread token: `Welcome`, then every pending `ApprovalRequest` of the thread, then stashed undelivered control frames in order; later replies to messages from the old connection reach the new connection through scope routing (8.6.4). The old connection must be gone; while it is alive, the new one gets `thread_in_use`.

#### 4.3.9 How `msg_id` travels through the agent

1. Ingress builds `IncomingMessage::new("unix_socket_repl", owner_id, content).with_sender_id(session_id).with_metadata({"unix_session_id", "repl_msg_id"}).with_attachments(…).with_thread("unix:<token>")` (v3 only for the thread). An approval message also carries `"repl_ctl_id"` (the `Approval` frame's id) and its `repl_msg_id` is the original message's id from the pending entry.
2. `prepare_message` mutates content and attachments only (`agent_loop.rs:1373-1385`); `id` and `metadata` survive into `supervise_message` and `deliver_message_result`.
3. Every V1 status producer passes the handled message's metadata (`thread_ops.rs`, `dispatcher.rs`, `commands.rs:44`); V2's `engine_status_metadata` passes non-gateway metadata unchanged (`router.rs:4106-4111`).
4. Replies go through `ChannelManager::respond(message, …)` with the handled message (`response_delivery.rs:46-101`, error path `agent_loop.rs:1413-1424`, soft timeout `:1520-1528`).
5. The lifecycle hook receives the same message. The channel reads `metadata["repl_msg_id"]` in `respond`, `send_status` and `message_lifecycle`; a message without it is unsolicited (8.6.4).

---

## 5. Voice design

### 5.1 Principles

- UX: dictate, the transcript is inserted at the composer cursor, the user edits, Enter sends. No auto-send. Live partials are out of scope: LunarVoice is batch-only (30 s offline chunks, no partials) and Hex partials need a GPU that is full on this host (llama-server holds 22.6 GB; about 580 MiB free).
- Audio is **never** sent as a chat attachment. That path transcribes inline on the main agent loop (`ic/src/agent/agent_loop.rs:1309-1317`, `:1373-1385`, up to 120 s per clip) and persists raw audio as base64 into the search-indexed workspace (`ic/src/agent/attachment_store.rs:26`, `:126-139`). The transcribe-then-edit path stores nothing.
- The TUI always records; every source in this release transcribes a WAV the TUI recorded: `lunarvoice` (either transport), `hex-app` and `hex-embedded` (Hex `POST /transcriptions`). One in-memory recording can therefore be retried on the next source, and kept for one manual retry, without asking the user to speak again (5.4.1). Hex-recorded sources (`/dictations`, the CLI) are deferred (13, Deferred): they would be chosen only when no local recorder works, which is never the case on this desktop (OpenTUI Audio, or `/usr/bin/parec`), and they carry the widest privacy footprint (Hex History, `live.ndjson`).
- Server STT is LunarVoice only (R11): the gateway proxies to the loopback sidecar, and REPLv3 `Transcribe` uses the same `VoiceApi`. A remote provider (OpenAI, chat_completions) is never used for dictation.
- Nothing about voice is silent: recording mode, elapsed time, source, retries and fallbacks are shown (5.4, 7.4, 3.12.3).

### 5.2 Interfaces (`tui/src/voice/types.ts`)

```ts
export type VoiceSourceId = "lunarvoice" | "hex-app" | "hex-embedded"
export interface LevelSample { rmsDb: number; peakDb: number; at: number }
export interface WavClip {
  readonly wav: Uint8Array            // 44-byte RIFF/PCM header + s16le mono samples
  readonly sampleRate: 16000; readonly durationMs: number
  readonly peakDb: number             // loudest 20 ms frame of the whole clip (5.5)
  readonly droppedFrames: number
  dispose(): void                     // zero-fills WAV and PCM buffers
}
export interface Transcript { text: string; source: VoiceSourceId; model?: string; language?: string; audioMs?: number; elapsedMs: number }
export interface SourceAvailability { available: boolean; reason?: string; detail?: string; checkedAt: number }
export interface VoiceSource {
  readonly id: VoiceSourceId; readonly label: string; readonly privacyNote: string
  probe(signal: AbortSignal): Promise<SourceAvailability>   // side-effect free; results cached by the manager (5.4)
  maxWavBytes(): number
  transcribe(clip: WavClip, signal: AbortSignal): Promise<Transcript>
  dispose(): Promise<void>
}
export type VoiceErrorKind = "unavailable" | "busy" | "rate_limited" | "timeout" | "no_speech" | "too_large"
  | "unsupported_audio" | "auth" | "bad_request" | "model_not_ready" | "processing_failed" | "cancelled" | "recorder" | "internal"
export class VoiceError extends Error {
  constructor(readonly kind: VoiceErrorKind, message: string,
    readonly opts: { source?: VoiceSourceId | "recorder"; status?: number; code?: string; retryAfterMs?: number } = {}) { super(message) }
}
export interface Recorder { readonly kind: "opentui" | "parec"
  available(): Promise<{ ok: true } | { ok: false; reason: string }>
  start(opts: { maxDurationMs: number; device?: string; onLevel: (l: LevelSample) => void
                onFirstChunk: () => void; signal: AbortSignal }): Promise<RecorderHandle> }
export interface RecorderHandle { readonly kind: "opentui" | "parec"; stop(): Promise<WavClip>; cancel(): void }

// Declared here (scaffold task) so state/model.ts and the reducer compile before voice/manager.ts and voice/ptt.ts (F6).
export type PttPhase = "idle" | "arming" | "recording" | "finishing"
export type PttMode = "provisional" | "toggle" | "hold-key" | "hold-inferred" | "hold-mouse"
export interface VoiceQueueItem { id: string; durationMs: number; state: "waiting" | "transcribing"
  source: VoiceSourceId | null; startedAt: number | null; attempt: number }
export interface VoiceUiState {
  phase: PttPhase; mode: PttMode | null; trigger: "key" | "mouse" | null
  recorder: "opentui" | "parec" | null; startedAt: number | null
  captureLive: boolean                // the first chunk arrived ("opening mic..." until then)
  level: { rmsDb: number; peakDb: number; heldPeakDb: number; clipping: boolean; noSignal: boolean } | null
  queue: readonly VoiceQueueItem[]    // at most 3: one transcribing, two waiting (5.4)
  retained: { durationMs: number; reason: VoiceErrorKind; until: number; retryAt: number | null } | null   // 5.4.1
  availability: Partial<Record<VoiceSourceId, SourceAvailability>>
  lastAttempts: readonly { source: VoiceSourceId; outcome: VoiceErrorKind | "ok"; ms: number }[]
  lastError: { kind: VoiceErrorKind; message: string } | null
}
```

### 5.3 Backends

#### 5.3.1 `lunarvoice` (`tui/src/voice/sources/lunarvoice.ts`)

- Probe: `unavailable("not connected")` unless `conn.kind === "ready"` (so a PTT press while offline says so at once, UX-03); then `ServerInfo.capabilities.transcribe` (gateway: capabilities `transcribe` feature, or `/api/voice/status.configured` on legacy gateways; REPLv3: `transcribe` in `Welcome.features`; legacy socket: never).
- `maxWavBytes()` = `limits.maxTranscribeWavBytes` (18 MiB gateway, 12 MiB REPLv3).
- Client rate window: 60 s sliding window, limit `limits.transcribePerMinute` (10); a full window yields `rate_limited` (with `retryAfterMs`) without a request.
- Call: `transport.transcribe({requestId, wav, signal})`; empty text → `no_speech`.
- Error mapping. Gateway: 429 → `rate_limited`; 503 with `voice sidecar is unavailable` → `busy`; 503 with `voice service is not configured` → `unavailable` (source disabled for the session); 504 → `timeout`; 413 → `too_large`; 422 → `processing_failed`; 400 → `bad_request`; 401 → `auth`; 500/502 and network errors → `unavailable`. REPLv3 `Error{req_id}`: `busy` → `busy`; `rate_limited` → `rate_limited`; `too_large` → `too_large`; `unavailable`, `feature_not_negotiated` → `unavailable`; `timeout` → `timeout`; `no_speech` → `no_speech`; `transcribe_failed` → `processing_failed`; `unsupported_media` → `unsupported_audio`.
- Optional (M4, open question A.8): `voice.lunarvoice_profile = "<gateway profile>"` lets a socket session use a gateway profile's `/api/voice/status` and `/api/voice/transcribe` only.

#### 5.3.2 Hex discovery (`tui/src/voice/sources/hex-locate.ts`, M1b)

Hex is not installed on this host; every Hex source is optional and probed (6 releases in about 64 hours; docs contradict the source).

```ts
export interface HexEndpoint { baseUrl: string; token: Secret; pid: number; origin: "app" | "embedded" }
export interface HexLocator {
  locateApp(signal: AbortSignal): Promise<HexEndpoint | { unavailable: string }>      // cached 5 s
  binary(): Promise<{ command: string[]; version: string } | { unavailable: string }>  // cached
  invalidate(): void
}
```

Running-app discovery (mirrors Hex's own checks, `hex:src/local_api/discovery.rs:105-123`, `:204-226`, `:260-266`, plus the listener-owner check):
1. Path `${HEX_APPLICATION_SUPPORT_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/hex}/local-api.json` (`hex:src/app_paths.rs:31-43`, `:89-91`).
2. `lstat`: regular file, owner `process.getuid()`, `(mode & 0o077) === 0`, ≤ 4 KiB; any failure → `unavailable` with the reason.
3. JSON `{port, token, apiVersion, pid}`: `apiVersion === "2"`, token `/^hex_[0-9a-f]{64}$/`, `1 <= port <= 65535`, `pid > 0`.
4. Liveness: `process.kill(pid, 0)`; `ESRCH` → stale; `EPERM` → not ours → refuse. Then `assertListenerOwner(new URL("http://127.0.0.1:<port>"), {uid: process.getuid(), label: "this user"})` (3.8.1) instead of a bare TCP connect (SEC-01): a stale discovery file whose port another uid now holds is refused before the token or any audio is sent. The same check runs before every Hex request.
5. `GET http://127.0.0.1:<port>/health` with the bearer token (300 ms) → `{version, apiVersion:"2"}`. Only `127.0.0.1` is ever contacted. `/capabilities` is not used in this release.

Binary discovery: `voice.hex.command` (argv array, default `["hex"]` via `Bun.which`, or `$LUNARWING_TUI_HEX_BIN`), then `hex --version` (2 s) must print `hex <major>.<minor>.<patch>`; 3.3.0 or later enables `hex-embedded`, older versions leave it `unavailable` ("Hex 3.3.0 or later required").

Model presence (no side effects): `GET /models?language=en` → entries with `installed`, `verified`, `downloadBytes` (`hex:src/local_api.rs:1053-1080`); ready means `installed && verified`. The TUI never calls `POST /models/{id}/prepare` (no hidden 731 MB download); it shows the install command instead.

Version note (UX-05): the mocks follow the source at dev `54a3a66`; the released `hex-3.3.0-linux-x64` binary contains the same wire strings (`hexbin:` `strings` shows `embedded-host-lease`, `model-not-ready`, `queue-full`, `service-gone`, `unknown-model`, `upload-timeout`, `audio/wav`). The supervised test 16.9 runs against the released asset, and `tui/README.md` records the tested version.

#### 5.3.3 `hex-app` and `hex-embedded`: the TUI records, `POST /transcriptions` (M1b)

```
POST /transcriptions?model=parakeet_unified_en&language=en HTTP/1.1
Authorization: Bearer hex_<64 hex>
Content-Type: audio/wav
Content-Length: 128044

<RIFF ... WAVE ... data ...>

200 {"transcript":"check the ports registry","durationMs":4000}
409 {"code":"model-not-ready"}   429 {"code":"queue-full"}   413 {"code":"resource-exhausted"}
```

- Every local-API route requires the bearer token (`hex:src/local_api.rs:577-582`). `model` is required (`hex:src/local_api.rs:893`); bodies need `Content-Length` (chunked bodies are refused); 64 MiB max; 30 s upload deadline.
- Model: `voice.hex.model` if set, else the first `verified` entry of `GET /models?language=<lang>`; none → `unavailable` with "run `hex model install parakeet_unified_en --language en`".
- `hex-app` uses the discovered app endpoint. `hex-embedded` spawns the hidden Linux subcommand when no app runs (`hex:src/linux.rs:64-71`, `:123-125`):
  - `Bun.spawn([...hexCommand, "service", "--embedded"], {stdin:"pipe", stdout:"pipe", stderr:"pipe", env: childEnv})` (argv, no shell; `TRANSCRIBE_DUMP_DIR` removed, it can write audio-derived tensors).
  - First stdout line within 15 s, at most 16 KiB: `{"type":"ready","url":"http://127.0.0.1:41873","token":"hex_…","apiVersion":"2","pid":12345}` (`hex:src/local_api.rs:273-281`). Validate `type`, `apiVersion`, URL `http://127.0.0.1:<port>` without credentials, token format, `pid === child.pid`, and `assertListenerOwner` for the port with `process.getuid()`; anything else kills the child.
  - stdin is the lifetime lease: kept open, never written; closing it makes the child exit within about 5 s (`hex:src/local_api.rs:321-343`), so a TUI that dies (even by `SIGKILL`) never leaves the child running. stdout/stderr are drained into 8 KiB rings and never shown raw.
  - Spawned lazily at the first transcription, kept for a 10 min idle TTL, disposed on quit: close stdin, `SIGTERM` after 6 s, `SIGKILL` after 10 s.
  - Embedded mode writes no discovery file and takes no global lock; `/streams` answers 503 and `/dictations` is unavailable; only `/transcriptions` and `/models` are used. It writes `logs/process.log` and a transient `logs/embedded-<pid>.ndjson` without transcript text.
- Errors: 409 `model-not-ready` → `model_not_ready`; 429 `queue-full` → `busy`; 413 → `too_large`; 415 → `unsupported_audio`; 400 `invalid-audio`/`unknown-model`/`unsupported-model`/`invalid-request` → `bad_request`; 408 → `timeout`; 500 → `processing_failed`; 503 `service-gone` → `unavailable`; 401 → invalidate discovery, re-locate once, then `unavailable`. Empty transcript → `no_speech`. Client timeout 90 s.

#### 5.3.4 Deferred Hex sources

`/dictations` (Hex records) and the CLI (`hex dictation start|stop|cancel`) are not built in this release (UX-09). Their requirements, for when they are scheduled, are in section 13 (Deferred): both headers on every `/dictations/{id}/*` call (F12) and a crash lease for the CLI (SEC-12).

### 5.4 Selection, background probes and automatic fallback (`tui/src/voice/manager.ts`)

Default `voice.order = ["lunarvoice", "hex-app", "hex-embedded"]` on both transports. Ids without an implementation in the running build (Hex before M1b) are skipped and listed in the voice panel as "not in this build". `/voice use <id>` pins one source for the session; `/voice auto` restores the order.

Background probes (UX-06): the manager probes every source (in parallel, 1 s budget each) when the transport becomes `ready`, every 30 s while the TUI runs, when the Hex discovery file's `mtime` changes (checked with `stat` every 5 s), and on `/voice`. Results land in `VoiceUiState.availability`. A key press never waits for a probe.

At push-to-talk start (the PTT machine emits `start_capture`), only the recorder is awaited:

```
if no source is available in the cache:  notice "voice unavailable: <reason per source>"; PTT back to idle
elif the queue already holds 3 clips:    notice "transcription queue full; wait or press Esc"; idle
else:                                    recorder.start() at once (OpenTUI Audio, else parec); "opening mic..." until the first chunk
```

At finish, the clip enters the queue (at most one transcribing and two waiting) and a new recording may start at once. The queue worker transcribes clips one at a time in order; each transcript is inserted at the cursor in that order:

```
candidates := [s in order | availability(s) (re-probed if older than 30 s) && s.maxWavBytes() >= clip.wav.length]
try candidates in order with the same WavClip; total budget 180 s per clip
```

| VoiceErrorKind | Next step |
|---|---|
| `unavailable` | next candidate; recorded in the attempt list; that source is re-probed after 30 s |
| `busy` | retry the same source after 1, 2 and 4 s (R18), then the next candidate |
| `rate_limited` | next candidate; if none is left, the clip is retained with `retryAt` = the end of the window |
| `timeout`, `processing_failed`, `model_not_ready` | next candidate (one-time hint "run `hex model install`" for `model_not_ready`) |
| `too_large` | next candidate with a larger limit |
| `auth` | next candidate; sticky warning about the gateway token (the transport handles the 401 separately) |
| `no_speech` | stop: notice "no speech detected"; the clip is disposed, never retried |
| `bad_request`, `unsupported_audio` | stop with the code; the clip is disposed (client bug) |
| `cancelled` | stop at once, abort the request, dispose the clip |
| `recorder` | at start: try `parec`, else "voice unavailable: <recorder reason>" |

Outcome notices (UX-03):
- Success from the first candidate: info "transcribed 14 s in 1.2 s (lunarvoice)".
- Success from a later candidate: warn "transcribed by hex-embedded (lunarvoice: busy)".
- End of chain (no candidate produced a transcript): one warn naming every attempt, for example "voice failed: lunarvoice busy (3 retries); hex-app not running; hex-embedded not installed", and the clip is retained (5.4.1).

Every attempt is recorded as `{source, outcome, ms}` for the voice panel. The clip is disposed after success and in the terminal cases above.

#### 5.4.1 Retained clip (UX-02)

- The controller keeps the last failed `WavClip` in memory (never in the store, never on disk); the store holds only `VoiceUiState.retained` metadata. The notice reads "transcription failed (lunarvoice busy): Alt+R retry, Esc discard".
- Alt+R and `/voice retry` rerun the chain of 5.4 with the same clip. For `rate_limited`, `retryAt` is the end of the client window (or the server's `retryAfterMs`); Alt+R before it shows "wait N s".
- The clip is zero-filled and dropped on success, on Esc (when no recording is active), when a new recording starts, on quit, and 10 min after the failure.

### 5.5 Recorder

**OpenTUI Audio (primary, `voice/recorder-opentui.ts`)**:
- One engine per process, created at startup unless `--no-voice`: `Audio.create({ sampleRate: 16000, autoStart: false })` (`ot081:audio.d.ts:4-9`, `:429`). `autoStart: false` starts no device, so no stream is open until a recording; this removes engine creation from the press-to-capture path (UX-06). Capture always uses the engine rate (default 48 000), so the rate must be set here. An initialization error marks the recorder unavailable.
- Per recording: `await audio.openCapture({ channels: 1, chunkFrames: 320, capacityFrames: 16000, signal })` → `AudioCaptureStream` whose `readable` is a `ReadableStream<Float32Array>` (`audio.d.ts:160-170`, `:322`, `:477`). 320 frames = 20 ms. `onFirstChunk` fires on the first chunk read, which emits the PTT event `capture_started` (7.3): a PipeWire source woken from suspend takes a moment, and only then is the mic live.
- A second `openCapture` while one is open throws; the recorder is a singleton.
- Stop: `capture.stop()` drains and closes the stream, and the `for await (const chunk of capture.readable)` loop ends. Cancel: `capture.dispose()`.
- `capture.getStats().framesDropped` (a `bigint`, `audio.d.ts:190`) above `0n` adds the warn notice "audio dropped: the UI was busy".
- Devices: `listCaptureDevices()` / `selectCaptureDevice(index)` (`audio.d.ts:479-480`). The device is chosen, in order, from the profile state file's `voiceDevice` (written by `/voice device <name>`), then `voice.device` (case-insensitive substring of the name), then the default source.
- PCM/WAV (`voice/wav.ts`): preallocated `Int16Array(maxDurationMs * 16)`; each Float32 sample clamped to [-1, 1] and stored as `round(s < 0 ? s * 32768 : s * 32767)`; `encodeWav(pcm, 16000)` writes the 44-byte header (`RIFF`, `36 + N`, `WAVE`, `fmt `, 16, PCM 1, 1 channel, 16000 Hz, byte rate 32000, block align 2, 16 bits, `data`, `N`) and little-endian samples via `DataView`.

**parec fallback (`voice/recorder-parec.ts`)**: `Bun.spawn(["parec", "--raw", "--format=s16le", "--rate=16000", "--channels=1", "--latency-msec=20", "--client-name=wingtip", "--stream-name=dictation", ...(device ? [`--device=${device}`] : [])], {stdin:"ignore", stdout:"pipe", stderr:"pipe", env: childEnv})`. Chunks are converted with an odd-byte carry; the first stdout chunk calls `onFirstChunk`. Stop: `SIGINT`, wait 1 s, `SIGKILL`, read the rest. Cancel: `SIGKILL`. `pw-record` is not used (it needs `--raw` and `stdbuf -o0`).

**Levels, signal checks and VAD**:
- `voice/levels.ts`: per 20 ms chunk `rmsDb = 20*log10(rms)`, `peakDb = 20*log10(peak)`, floor -90 dBFS. `components/vu-meter.tsx`: -60..0 dBFS in 12 cells, instant attack, about 25 dB/s release, 1 s peak hold, clip marker at ≥ -1 dBFS, "no signal" after 1.5 s below -70 dBFS (the C922 source on this host is muted); throttled to 15 Hz (4 Hz with `reduce_motion`).
- Local discard is based on signal, never on the VAD (UX-02): a clip under 300 ms is discarded ("too short"); a clip whose `peakDb` stayed below -50 dBFS for its whole length is discarded with "no signal from <device> - /voice device". Every other clip is sent; an empty server transcript means `no_speech`.
- `voice/vad.ts` serves only the opt-in toggle auto-stop (`auto_stop_on_silence`, default off): noise floor = 10th-percentile RMS of the first 15 frames, clamped to [-80, -35] dBFS, then `floor = 0.95*floor + 0.05*rms` while `rms < floor + 6`; speech when `rms > max(floor + 12, -50)`; speech has started after 5 speech frames among the last 10; after speech, `silence_stop_ms` (default 3500) of non-speech emits `silence`.
- In toggle mode, no frame above -50 dBFS within 8 s emits `no_signal_timeout` (cancel with "no signal from <device>").
- Durations: `voice.max_duration_s` default 120, hard cap 290 (the sidecar processes at most 300 s), further capped by the largest `maxWavBytes()` among available sources: `floor((maxWavBytes - 44) / 32000)` s. Warning 10 s before the cap.

### 5.6 Privacy

| Path | What persists | Source |
|---|---|---|
| TUI recorder | nothing on disk: PCM and WAV exist only in memory; zero-filled after success, cancel, or when the retained clip is dropped (at most one failed clip, at most 10 min, 5.4.1); no temp files (`AudioRecorder.recordToFile` is not used); the capture stream is opened per recording | 5.5 |
| LunarVoice (gateway or REPLv3) | no daemon persistence (the preview path never touches `attachment_store`); the sidecar keeps a 600 s in-memory cache keyed by the audio hash; audio stays on the host (loopback sidecar) | `ic/src/channels/web/voice.rs`; `projects/voice-sidecar/src/api.rs:455-462` |
| Hex `/transcriptions` (app or embedded) | no WAV, no history, no transcript in event logs; `process.log` is written; native dump flags scrubbed by the TUI | `hex:docs/specs/local-transcription-service.md:129-136` |
| Images (all transports, SEC-09) | every image the TUI sends (clipboard, `/img`, drag-and-drop, `/shot`) is stored as base64 under `attachments/<channel>/<id>/` in the tenant's search-indexed workspace, with no expiry, readable through `attachment_store` and `memory_read` from every channel and the web UI; on multi-tenant hosts that is the tenant uid's storage, readable by that tenant's agent and tools | `ic/src/agent/agent_loop.rs:1381-1383`; `ic/src/agent/attachment_store.rs:1-11`, `:126-133` |
| Clipboard | cliphist records image and text copies on this host, including the TUI's own copies (3.12.4); the TUI cannot prevent it | crosscheck_external |
| Hex app dictation (`/dictations`, CLI) | not used in this release (would keep Hex History for one week and `live.ndjson`) | `hex:src/history.rs:35-44`; `hex:src/linux/dictation.rs:806-830` |

Surfacing: the first use of each voice backend and of each image source (clipboard, file, drop, shot) shows a one-line notice with its privacy note; the acknowledgements are stored in the profile state file (`voiceAck`, `imageAck`). The image notice reads "images you send are stored in <tenant>'s workspace (no expiry) and are readable from every channel". The attachment bar caption ends with "stored in <tenant>'s workspace". `/voice privacy` opens the privacy panel. Transcripts never touch the clipboard. The debug log never records transcripts or audio. Whether TUI image turns should skip `store_attachments` is decision DEC-3.

### 5.7 VoiceController (`tui/src/voice/manager.ts`)

```ts
export interface VoiceController {
  pttKey(e: { eventType: "press" | "release"; repeated: boolean }): void
  micDown(): void; micUp(): void
  escape(): boolean                 // true if it cancelled the recording, the queue, or dropped the retained clip
  retry(): boolean                  // Alt+R, /voice retry (5.4.1)
  blur(): void; focus(): void
  probeNow(): void                  // /voice, reconnect
  status(): VoiceStatusReport
  use(id: VoiceSourceId | "auto"): void; setOrder(ids: VoiceSourceId[]): void
  listDevices(): Promise<{ index: number; name: string; isDefault: boolean }[]>
  selectDevice(name: string): Promise<boolean>   // persists voiceDevice in the profile state file
  dispose(): Promise<void>
}
export function createVoiceController(deps: { store: Store; clock: Clock; composer: ComposerHandle;
  recorders: Recorder[]; sources: VoiceSource[]; config: VoiceConfig; stateFile: StateFile }): VoiceController
// voice/sources/index.ts: createVoiceSources(deps): VoiceSource[]; M1a returns [lunarvoice], M1b adds the Hex sources
```

`Store` comes from `state/model.ts`, `ComposerHandle` from `controller/types.ts` and `StateFile` from `config/types.ts`, all scaffold types (F6). The controller runs `pttReduce` (section 7) on every input and on a 50 ms `Clock` tick while not idle, executes the emitted effects, runs the transcription queue and the background probes, and dispatches `voice/state` and throttled `voice/level`. Dictation is disabled in credential mode.

---

## 6. Images

### 6.1 Input gestures (client)

- **Ctrl+V** (`tui/src/images/clipboard.ts`):
  1. OpenTUI host clipboard, created once: `createHostClipboard({ timeoutMs: 1500, maxReadBytes: limits.maxImageBytes })` (`ot081:lib/clipboard.d.ts:90-109`); `read({preferredTypes: ["image/png","image/webp","image/jpeg","image/gif"], selection: "clipboard", signal})`. `read` → image; `limit-exceeded` → "clipboard image larger than 5 MiB"; `empty`/`unsupported` → step 2; `failed`/`timed-out` → step 3. Preferred types must be bare MIME essences.
  2. `read({preferredTypes: ["text/uri-list"]})` → copied files; then `["text/plain"]` → text inserted at the cursor (sanitized).
  3. Fallback only when the host clipboard failed and `WAYLAND_DISPLAY` is set: `wl-paste --list-types` (1.5 s), first offered of the four image types, `text/uri-list`, `text/plain*`, then `wl-paste --no-newline --type <type>` with a stdout cap of `maxImageBytes + 1` (child killed when exceeded) and 3 s timeout. `xclip` only if present; `xsel` dropped (text only).
  4. All `Bun.spawn`, no `spawnSync` anywhere under `src/` (a test greps the sources). The clipboard is read only on an explicit Ctrl+V.
  5. The first image from each source (clipboard, file, drop, shot) shows the storage notice of 5.6 once per profile (`imageAck`, SEC-09).
- **`/img <path>`, `/attach <path>`** (`images/attach.ts`, `readImageFile(path, {maxImageBytes, cwd})`): trim, strip one pair of quotes, expand `~/`, accept `file:///…` and `file://localhost/…` (percent-decoded), resolve relative to the working directory; `stat` must report a regular file within the limit; the read itself is bounded to `maxImageBytes + 1` bytes; filename = basename, ≤ 128 characters.
- **Drag and drop** (`images/paste-paths.ts`, `parseDroppedPaths(text, {maxPaths})`): kitty delivers a drop as newline-joined paths through bracketed paste, which tmux forwards. The global `usePaste` handler (`otr081:src/hooks/use-paste.d.ts`) decides synchronously: a paste ≤ 64 KiB with at most `maxImages` non-empty lines, each an absolute path or file URI ending in `.png`, `.jpg`, `.jpeg`, `.gif` or `.webp` (case-insensitive; backslash-escaped and single-quoted forms accepted), calls `event.preventDefault()` and attaches asynchronously; if any attach fails, a notice explains why and the original text is inserted. Other pastes go to the composer with their line breaks (3.10.5).
- **`/shot [full]`** (`images/screenshot.ts`, UX-08): requires `WAYLAND_DISPLAY`, `grim` and `slurp`. On this host grim's default scale is the greatest output scale (1.5 from HDMI-A-1), so a full capture of the 6400x1440 logical layout comes out near 9600x2160 and a PNG of it cannot fit 5 MiB; every capture therefore pins the scale to 1.
  - Region: `["slurp"]` (60 s; exit 0 yields one line matching `/^-?\d+,-?\d+ \d+x\d+$/`; non-zero exit is a quiet cancel), then `["grim", "-s", "1", "-g", geometry, "-"]`.
  - `full`: `["slurp", "-o"]` (the user clicks one output; same validation), then the same `grim` call. Picking the output is the deliberate second step for a full-screen capture.
  - Each `grim` run has a 10 s timeout and a stdout cap of `limits.maxImageBytes + 1` bytes (the child is killed when exceeded). A PNG over the limit is retried once with `["grim", "-s", "1", "-t", "jpeg", "-q", "85", "-g", geometry, "-"]`; the result must sniff as PNG, or as JPEG when JPEG was requested. Still too large: "screenshot is N MiB after JPEG; select a smaller region".
  - No shell, no clipboard, no temp file. Recommended over clipboard paste because cliphist records clipboard images. The captured image appears in the attachment bar and is sent only with the next Enter.

### 6.2 Validation (client and server use the same rules)

| Type | Signature | Accepted |
|---|---|---|
| PNG | `89 50 4E 47 0D 0A 1A 0A` | yes |
| JPEG | `FF D8 FF` | yes |
| GIF | `GIF87a` or `GIF89a` | yes |
| WebP | `RIFF ???? WEBP` | yes |
| BMP, SVG/XML, TIFF, HEIC, AVIF, ICO, anything else | | no: "unsupported image type (allowed: PNG, JPEG, GIF, WebP)" |

- Client (`images/sniff.ts`, `images/limits.ts`): the sniffed type is the only type sent; clipboard types, extensions and data-URL prefixes are never trusted. `imageInfo(bytes)` (`ot081:image.d.ts:84`) must succeed with width and height ≥ 1 and `width*height` ≤ 40 megapixels. Limits from `ServerInfo.limits`, checked at attach time and again at send time. No client-side downscaling (OpenTUI exposes no encoder); an oversize image is rejected with "select a smaller region (/shot) or compress it".
- Server (NEW `ic/src/channels/image_mime.rs`, M2a; used by the gateway in M2a and the socket in M4):

```rust
pub(crate) const IMAGE_MIME_ALLOWLIST: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
pub(crate) fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str>;     // magic bytes above; None otherwise
pub(crate) fn extension_for(mime: &str) -> &'static str;                  // png | jpg | gif | webp
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageLimits { pub max_count: usize, pub max_bytes: usize, pub max_total_bytes: usize }
pub(crate) const GATEWAY_IMAGE_LIMITS: ImageLimits = ImageLimits { max_count: 5, max_bytes: 5 * 1024 * 1024, max_total_bytes: 7 * 1024 * 1024 };
pub(crate) const SOCKET_IMAGE_LIMITS: ImageLimits = ImageLimits { max_count: 5, max_bytes: 5 * 1024 * 1024, max_total_bytes: 10 * 1024 * 1024 };
pub(crate) struct ImageInput<'a> { pub declared_mime: &'a str, pub data_b64: &'a str, pub filename: Option<&'a str> }
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ImageRejection {
    #[error("{count} images (max {max})")] TooMany { count: usize, max: usize },
    #[error("image {index} exceeds {max} bytes")] TooLarge { index: usize, max: usize },
    #[error("images exceed {max} bytes in total")] TotalTooLarge { max: usize },
    #[error("image {index}: invalid base64")] BadBase64 { index: usize },
    #[error("image {index}: unsupported image type (allowed: png, jpeg, gif, webp)")] Unsupported { index: usize },
    #[error("image {index} is empty")] Empty { index: usize },
}
/// Count and base64 lengths are checked before any decoding (D4); content is sniffed and the
/// sniffed type replaces the declared one (D6); ids are `<id_prefix>-<uuid v4>` (D7);
/// filenames are sanitized to <= 128 chars without control characters or path separators (D12).
pub(crate) fn decode_images(inputs: &[ImageInput<'_>], limits: ImageLimits, id_prefix: &str)
    -> Result<Vec<crate::channels::IncomingAttachment>, ImageRejection>;
```

Order inside `decode_images`: count; total base64 length against `4*ceil(max_total/3)`; per item: base64 length against `4*ceil(max_bytes/3)`, declared type must start with `image/` (ASCII case-insensitive), decode, non-empty, decoded ≤ `max_bytes`, running total ≤ `max_total_bytes`, sniff, allowlist; then build the attachment (`kind: Image`, `mime_type` sniffed, `filename` sanitized or `image-<i>.<ext>`, `size_bytes`). A declared/sniffed mismatch within the allowlist is accepted with a `debug!` log.

Mapping: gateway `TooLarge`/`TotalTooLarge` → 413, others → 422, body `image <i> rejected: <reason>`; `ws.rs` sends `WsServerMessage::Error`. REPLv3 `TooMany`/`TooLarge`/`TotalTooLarge` → `too_large`, `BadBase64` → `bad_frame`, `Unsupported`/`Empty` → `unsupported_media`.

### 6.3 Encoding per transport

Gateway: `images:[{"media_type":<sniffed>,"data":<base64>}]`. REPLv3: `attachments:[{"mime_type":<sniffed>,"filename":<name>,"data_base64":<base64>}]` (only with feature `attachments`). Legacy socket: refused before sending.

### 6.4 Rendering

- Pending attachments: `components/attachment-bar.tsx` (20×7 cells, `fit="cover"`, caption like `shot.png 412 KiB 1280x720`); one line (`[2 images] Ctrl+R remove last, Ctrl+X clear`) below 16 rows.
- Sent images under the user's input; generated images (`turn.image`) under the reply, at most 56 columns × 12 rows, `fit="fit"`.
- Generated images: only `data:image/(png|jpeg|gif|webp);base64,` URLs up to 16 MiB of base64 are decoded (`images/data-url.ts`), and the bytes must pass `images/sniff.ts`. The source handed to `<image>` is always the validated `Uint8Array`. A server `path` is shown as caption text and never passed as `ImageRenderable.source`, because `ImageSource` accepts `string | URL` and would make OpenTUI read a file or fetch a URL (`ot081:image.d.ts:8`).
- Protocol: `--image-protocol auto|kitty|sixel|blocks` and `LUNARWING_TUI_IMAGE_PROTOCOL`; the header shows `resolveImageRenderProtocol(requested, renderer.capabilities, renderer.resolution !== null)` (`ot081:renderables/Image.d.ts:16`). In tmux OpenTUI returns blocks unless forced; forcing kitty in tmux misplaces images (OpenTUI #1690) and `/transport` says so.
- Memory: bytes for the newest 20 images; older ones become placeholders.
- Full size (UX-14, `images/export.ts`): inside tmux images are block thumbnails of at most 56x12 cells. `/image open [n]` writes the validated bytes of the newest (or n-th newest) generated image to `$XDG_RUNTIME_DIR/wingtip/img-<uuid>.<ext>` (directory created 0700 and checked with `lstat` for owner and mode; file opened `O_CREAT | O_EXCL | O_NOFOLLOW | O_WRONLY`, mode 0600) and spawns `["xdg-open", file]` (argv only, child environment); exported files are removed on quit. `/image save [n] [path]` writes a 0600 file the same way (`O_EXCL`), default `./lunarwing-<tenant>-<YYYYmmdd-HHMMSS>-<n>.<ext>`. Both refuse an image whose bytes were already evicted ("image no longer in memory").

### 6.5 Vision caveat

Image parts reach the model only in the turn they are sent with on V1 (`ic/src/agent/thread_ops.rs:593-610`; cleared in `session.rs:707-737`); V2 carries them as transient parts. There is no vision capability gate, and LunarVision enrichment is XMPP-only. The default TensorZero `lunarwing` function routes about 99 % of traffic to Kimi K3; whether it accepts images is unverified (supervised test 16.5). Until verified, the README states that images are delivered to the configured model, which may ignore them, and the first image send of a session shows the info notice "images go to the configured model; vision is not verified for <tenant>" (repeated in the transport panel) unless the tenant is listed in `[ui] vision_verified_tenants` (UX-05). Test 16.5 is part of M1a's acceptance; if it fails, decision A.19 becomes an M1a exit criterion.

---

## 7. Push-to-talk (`tui/src/voice/ptt.ts`)

### 7.1 Inputs per environment (verified by the cross-checks)

| Environment | Key press | Key release | Key repeat | Mouse press/release | Focus out |
|---|---|---|---|---|---|
| kitty direct, KKP negotiated | yes | yes (`eventType:"release"`) | `repeated:true` | yes | yes |
| kitty inside tmux 3.7c (Christopher's setup) | yes; Ctrl+Space arrives as NUL | **no** (tmux never answers the KKP query) | presses every 20 ms after 300 ms | **yes** (SGR press and release forwarded) | yes (`focus-events on`) |
| other terminals without KKP | yes | no | yes | usually | depends |

OpenTUI maps NUL to `name:"space", ctrl:true` and the KKP form `ESC[32;5u` to the same name; F9 is `ESC[20~` (`ot081:testing/mock-keys.d.ts:24`) and its KKP release `ESC[20;1:3~`. `useKittyKeyboard: {events: true}` is harmless inside tmux because KKP flags are pushed only when the terminal answers the query. Focus-out arrives as `ESC[O` → blur.

### 7.2 Types

```ts
import type { PttPhase, PttMode } from "./types"   // declared in voice/types.ts (5.2)
export type PttTrigger = "key" | "mouse"
export interface PttConfig {
  holdThresholdMs: number     // 300: shorter press/click = tap (toggle)
  repeatDelayMs: number       // 600 default; 300 matches Hyprland repeat_delay on this host
  repeatQuietMs: number       // 250: no repeat for this long = key released (repeat interval 20 ms)
  minClipMs: number           // 300
  maxDurationMs: number       // 120000 (hard cap 290000)
  blurPolicy: "hold" | "always" | "never"
  autoStopOnSilence: boolean  // toggle modes only; default false (UX-06)
  noSignalTimeoutMs: number   // 8000: toggle recording with no frame above -50 dBFS is cancelled
}
export interface PttState {
  phase: PttPhase; mode: PttMode | null; trigger: PttTrigger | null
  pressAt: number | null; lastKeyAt: number | null; repeats: number
  releaseConfirmed: boolean   // a KKP release for a PTT key was seen in this process
  startedAt: number | null; warnedNearMax: boolean
}
export type PttEvent =
  | { type: "key_press"; repeated: boolean; at: number } | { type: "key_release"; at: number }
  | { type: "mouse_down"; at: number } | { type: "mouse_up"; at: number } | { type: "tick"; at: number }
  | { type: "escape"; at: number } | { type: "blur"; at: number }
  | { type: "capture_started"; at: number } | { type: "capture_failed"; at: number }
  | { type: "silence"; at: number } | { type: "no_signal_timeout"; at: number }
  | { type: "finished"; at: number }
export type PttEffect =
  | { type: "start_capture"; trigger: PttTrigger }
  | { type: "finish_capture"; reason: "toggle" | "release" | "release_inferred" | "silence" | "max_duration" | "blur" | "stuck" }
  | { type: "cancel_capture"; reason: "escape" | "too_short" | "no_signal" | "capture_failed" }
  | { type: "notice"; level: "info" | "warn"; text: string }
export function pttReduce(s: PttState, e: PttEvent, cfg: PttConfig): { state: PttState; effects: PttEffect[] }
```

The PTT machine covers capture only. Transcription runs in the controller's queue (5.4), so a new recording can start while earlier clips are transcribed.

### 7.3 Transitions

`arming` and `recording` share input rules; a finish or cancel decided during `arming` is queued and executed once the capture is open. A press is **fresh** when `!repeated && (lastKeyAt === null || at - lastKeyAt >= repeatQuietMs)`, otherwise a **repeat**.

| Phase / mode | Event | Result |
|---|---|---|
| idle | `key_press` fresh | `arming`, mode `provisional`, trigger key, `pressAt = lastKeyAt = at`; `start_capture(key)` (the controller may refuse: no source, or queue full, 5.4) |
| idle | `key_press` repeat | ignored (key still held after Esc) |
| idle | `mouse_down` | `arming`, `provisional`, trigger mouse; `start_capture(mouse)` |
| arming | `capture_started` (first captured chunk, 5.5) / `capture_failed` | `recording`, `startedAt = at` / `idle` + `cancel_capture(capture_failed)` |
| provisional, key | `key_press` repeated, or within `repeatDelayMs + repeatQuietMs` of `pressAt` | `repeats++`, `lastKeyAt = at`; mode `hold-key` if `releaseConfirmed`, else `hold-inferred` |
| provisional, key | `key_release` | `releaseConfirmed = true`; held ≥ `holdThresholdMs` → `finish_capture(release)`; else mode `toggle` |
| provisional, key | `tick`, no repeat and `at - pressAt > repeatDelayMs + repeatQuietMs` | mode `toggle` |
| provisional, mouse | `mouse_up` | held ≥ `holdThresholdMs` → `finish_capture(release)`; else `toggle` |
| provisional, mouse | `tick` with `at - pressAt >= holdThresholdMs` | mode `hold-mouse` (label) |
| hold-inferred | `key_press` repeat / `tick` with `at - lastKeyAt > repeatQuietMs` / `key_press` fresh / `key_release` | `lastKeyAt = at` / `finish_capture(release_inferred)` / `finish_capture(stuck)` / `releaseConfirmed = true` + `finish_capture(release)` |
| hold-key | `key_release` / `key_press` fresh | `finish_capture(release)` / `finish_capture(stuck)` |
| hold-mouse | `mouse_up` | `finish_capture(release)` |
| any hold mode | fresh press from the other trigger | `finish_capture(stuck)` |
| toggle | `key_press` fresh or `mouse_down` / `key_press` repeat / `silence` (only with `autoStopOnSilence`) / `no_signal_timeout` | `finish_capture(toggle)` / `lastKeyAt = at` / `finish_capture(silence)` / `cancel_capture(no_signal)` + info "no signal from <device>" |
| any recording mode | `tick` at `maxDurationMs - 10000` (once) / at `maxDurationMs` | warn "10 s left" / `finish_capture(max_duration)` + notice |
| any recording mode | `blur` | hold modes and provisional: `finish_capture(blur)` unless `blurPolicy === "never"`; toggle only when `"always"` |
| arming, recording, finishing | `escape` | `idle`; `cancel_capture(escape)`; info "dictation cancelled" |
| any `finish_capture` | rule | if `at - startedAt < minClipMs`: `cancel_capture(too_short)` instead |
| finishing | `finished` (the recorder handed over the clip; it is queued) | `idle` |
| finishing | `key_press` fresh, `mouse_down` | ignored (the recorder is flushing, well under 1 s) |

A lost release is ended by the inferred-release timer, any fresh press, focus-out, the maximum duration, or Esc. Esc while the PTT machine is idle goes to the controller's `escape()` (queue, then retained clip; 3.10.4).

### 7.4 Bindings, mic widget and voice status

- `voice.ptt_keys = ["f9", "ctrl+space"]` (both free in Hyprland, tmux and kitty here). The global `useKeyboard(handler, {release: true})` maps them to `pttKey()` and calls `key.preventDefault()`.
- `components/mic-widget.tsx` at the left of the composer row. Every label fits 8 columns, so nothing wraps (`<text>` wraps words by default, `ot081:chunk-bun-ved2tpxm.js:14213`):

```tsx
<box style={{ width: 10, height: 1, flexShrink: 0 }}
     onMouseDown={(e) => { e.preventDefault(); voice.micDown() }}   // keeps focus in the composer
     onMouseUp={() => voice.micUp()}
     onMouseDragEnd={() => voice.micUp()}>
  <text selectable={false} wrapMode="none">{label}</text>   {/* "( mic )", "( ... )", "REC 0:07", "[ .... ]", "(no mic)" */}
</box>
```

- Labels: idle `( mic )`; arming `( ... )`; recording `REC m:ss`; transcribing (queue not empty, not recording) `[ .... ]`; no source available `(no mic)`.
- `components/vu-meter.tsx`: 12 cells to the right of the mic widget, only while recording; the no-signal state reads "no signal from <device> - /voice device" in the composer title.
- Voice status line (UX-03): while voice is active the composer's border title shows exactly one of: `opening mic...` (arming); `REC 0:07 - F9 stop - Esc cancel` (toggle); `REC 0:07 - release to stop - Esc cancel` (hold modes, including hold-inferred); `transcribing 0:03 - lunarvoice - Esc cancel`; `lunarvoice busy - retry 2/3 in 2 s`; `2 clips waiting`. Hold and toggle are told apart only here inside tmux (key repeat decides the mode), so the label must always be shown.
- The root `<box onMouseUp={() => voice.micUp()}>` catches releases elsewhere (mouse events bubble unless stopped); `micUp()` is idempotent. `enableMouseMovement: false` selects drag tracking.
- The momentary-KKP-through-tmux-passthrough trick is excluded as too fragile (open question A.10).

### 7.5 Tests

- `tui/test/voice/ptt.test.ts` (pure): "tap F9 starts, second tap stops (toggle)"; "held F9 under tmux: repeats after 300 ms then 250 ms quiet finishes (release_inferred)"; "first autorepeat at 300 ms is not taken as a stop tap"; "KKP: press, repeats, release at 600 ms finishes with reason release and sets releaseConfirmed; the next hold uses hold-key"; "KKP: release within 300 ms becomes toggle"; "mouse click toggles, mouse hold finishes on release"; "Esc cancels in arming, recording and finishing"; "blur finishes hold modes, not toggle (policy hold)"; "fresh press during hold-inferred finishes as stuck"; "max duration finishes with a notice; 10 s warning once"; "finish under 300 ms becomes cancel too_short"; "repeat in idle after Esc is ignored"; "a 2 s pause does not stop a toggle recording by default"; "no_signal_timeout cancels a silent toggle recording"; "REC appears only after capture_started".
- `tui/test/voice/queue.test.ts` (controller with fake recorder and sources): "press starts capture without awaiting probes"; "a second dictation during transcription inserts in order"; "a fourth clip is refused with queue full"; "Esc while idle cancels the queue, then drops the retained clip".
- `tui/test/ui/ptt-ui.test.tsx` (OpenTUI `testRender` with one `ManualClock` shared by renderer and controller, fake recorder and sources): "F9 toggle inserts the transcript into the composer and the composer never receives F9"; "NUL behaves as Ctrl+Space" (`renderer.stdin.emit("data", Buffer.from([0x00]))`); "KKP release ends a hold" (`testRender(app, {kittyKeyboard: true})`, press `\x1b[20~`, release `\x1b[20;1:3~`); "mouse hold on the widget, release outside the widget" (`mockMouse.pressDown` then `mockMouse.release` elsewhere, `ot081:testing/mock-mouse.d.ts:31-32`); "mouse click toggles"; "blur ends a mouse hold" (`\x1b[O`); "Esc during transcribing aborts the fake request"; "VU meter renders levels from the fake recorder".
- `tui/test/ui/voice-status.test.tsx`: "toggle and hold labels differ"; "REC timer visible at 80x24"; "busy retry countdown shown"; "fallback success names source and reason"; "offline gateway: PTT start says voice unavailable"; "unavailable at the end of the chain raises one notice and retains the clip".

---

## 8. Daemon changes

### 8.1 Change map

| Change | Milestone | Main files |
|---|---|---|
| Shared vocabulary (`TurnOutcome`, `WireLimits`, ids, features) | M2a | `ic/crates/lunarwing_common/src/client_caps.rs` (NEW) |
| Lifecycle hook + outcome hints | M2a | `ic/src/channels/{lifecycle.rs (NEW), channel.rs, manager.rs, mod.rs}`, `ic/src/agent/agent_loop.rs`, `ic/src/bridge/router/outcome.rs`, `ic/src/bridge/router.rs` |
| `/quit` refusal (gateway, socket) and the `Ok(None)` guard (DEC-1) | M2a | `ic/src/agent/commands.rs`, `agent_loop.rs` |
| Gateway correlation (including credential continuations), lifecycle events, capabilities, image validation, voice exposure | M2a | `ic/src/channels/web/{sse,types,mod,server,ws,voice}.rs`, `ic/src/channels/image_mime.rs` (NEW) |
| Gateway client tokens (store, middleware, stream revocation, credential-route rule) | M2b | `ic/src/channels/web/{client_tokens.rs (NEW), auth.rs, mod.rs, server.rs}`, `ic/src/main.rs` |
| V2 `ImageGenerated` | M2a | `ic/src/bridge/{generated_images.rs (NEW), effect_adapter.rs, router.rs}`, `ic/src/tools/builtin/image_gen.rs`, `ic/src/agent/dispatcher.rs` |
| Socket hardening on protocol 1 (WI#458 1-3, D1, D2, D3-v1, D5, D8, D9) | M3 | `ic/src/channels/unix_socket_repl/` (directory module replaces the file), `ic/crates/lunarwing_common/src/repl_wire.rs` (NEW) |
| `ChannelManager` lock release (D1 root) | M3 | `ic/src/channels/manager.rs` |
| `mount_guard` socket masking (WI#458 4) | M3 | `ic/src/sandbox/mount_guard.rs` |
| REPLv3 protocol (D3, D4, D6, D7, D10, D11, D12, threads, approvals, transcribe, notices) | M4 | `ic/src/channels/unix_socket_repl/*`, `ic/src/main.rs`, `ic/src/agent/submission.rs` (debug line only) |

`ic/src/app.rs` is untouched. `ic/src/main.rs` changes only by wiring (M2b: one `client_tokens::store_from_env` call; M4: `VoiceApi` shared with the socket). No new crates (`nix`, `subtle`, `uuid`, `base64`, `tokio-util`, `tempfile`, `futures`, `chrono` are already dependencies, `ic/Cargo.toml:78-219`), no ports-registry change, no new listener.

### 8.2 Message lifecycle hook and outcome hints (M2a)

#### 8.2.1 Types and trait method

```rust
// ic/src/channels/lifecycle.rs (NEW)
pub use lunarwing_common::TurnOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLifecycle { Started, Finished(TurnOutcome) }

/// Outcome refinements recorded by the code that knows (V1 result arms, V2 completion,
/// V2 detach) and applied once when the agent reports `Finished`. Bounded, std lock,
/// never held across `.await`.
pub(crate) struct OutcomeHints { inner: std::sync::Mutex<std::collections::VecDeque<(uuid::Uuid, TurnOutcome, std::time::Instant)>> }
impl OutcomeHints {
    pub(crate) const MAX_ENTRIES: usize = 1024;
    pub(crate) const TTL: std::time::Duration = std::time::Duration::from_secs(1800);
    pub(crate) fn note(&self, message_id: uuid::Uuid, hint: TurnOutcome);     // replaces an older hint for the id
    /// Hints replace only `Replied` and `NoReply`; `Failed`, `TimedOut` and `Cancelled`
    /// computed by the agent loop are authoritative. The hint is removed.
    pub(crate) fn resolve(&self, message_id: uuid::Uuid, computed: TurnOutcome) -> TurnOutcome;
}
```

```rust
// ic/src/channels/channel.rs, in `trait Channel` after `send_status` (channel.rs:511-518)
/// Agent-side lifecycle of a message this channel produced. `Started` fires when the agent
/// dequeues it; `Finished` fires after any reply was handed to `respond()`, exactly once per
/// message, except that `Finished(Detached)` is followed by exactly one more `Finished` from
/// the late watcher. Priority controls and scheduler rejections may report `Finished` without
/// `Started`. Must not block. Default: no-op.
async fn message_lifecycle(&self, _msg: &IncomingMessage, _event: MessageLifecycle) {}
```

```rust
// ic/src/channels/manager.rs
pub struct ChannelManager { /* existing fields */ outcome_hints: crate::channels::lifecycle::OutcomeHints }
impl ChannelManager {
    pub fn note_turn_outcome(&self, message_id: uuid::Uuid, hint: TurnOutcome);
    /// Applies hints to `Finished`, then delegates through `get_channel` (Arc clone; no lock
    /// held across the channel await, `manager.rs:397-399`).
    pub async fn message_lifecycle(&self, msg: &IncomingMessage, event: MessageLifecycle);
}
```

`ic/src/channels/mod.rs` gains `pub mod lifecycle;`, `pub(crate) mod image_mime;`, `pub use lifecycle::{MessageLifecycle, TurnOutcome};`, `pub const GATEWAY_CHANNEL: &str = "gateway";` and `pub const UNIX_SOCKET_REPL_CHANNEL: &str = "unix_socket_repl";` (no cfg gate, so `agent/commands.rs` can use them everywhere). Wrapper delegation (review-discipline rule): `SharedWasmChannel` (`ic/src/channels/wasm/wrapper.rs:3444`) and `TestChannelHandle` (`ic/tests/support/test_channel.rs:268`) forward the new method; `TestChannel` records lifecycle events (`captured_lifecycle() -> Vec<(Uuid, MessageLifecycle)>`).

#### 8.2.2 Agent loop calls (`ic/src/agent/agent_loop.rs`)

```rust
fn turn_outcome(result: &Result<Option<String>, Error>) -> TurnOutcome {
    match result {
        Ok(Some(text)) if !text.is_empty() => TurnOutcome::Replied,
        Ok(_) => TurnOutcome::NoReply,      // includes Ok(None) (shutdown) and the empty sentinels
        Err(_) => TurnOutcome::Failed,
    }
}
async fn report_started(&self, message: &IncomingMessage) {
    self.channels.message_lifecycle(message, MessageLifecycle::Started).await;
}
async fn report_finished(&self, message: &IncomingMessage, outcome: TurnOutcome) {
    self.channels.message_lifecycle(message, MessageLifecycle::Finished(outcome)).await;
}
```

| Site | Change |
|---|---|
| `supervise_message` (`:1326`), first statement | `self.report_started(&message).await;` |
| `Completed(Ok(result))` arm (`:1353-1355`) | `let outcome = turn_outcome(&result); let control = self.deliver_message_result(&message, result).await; self.report_finished(&message, outcome).await; control` |
| panic arm (`:1356-1359`) | after `handle_message_panic`: `report_finished(Failed)` |
| soft-timeout arm (`:1360-1365`) | after `handle_soft_timeout` and `disarm()`: `report_finished(TimedOut)` |
| cancelled arm (`:1366-1369`) | after `abort_and_wait`: `report_finished(Cancelled)` |
| `handle_priority_interrupt` (`:1387-1391`) | `report_started`, compute the outcome, deliver, `report_finished` |
| cancel-lane branch (`:1238-1252`) | `scheduler.cancel_lane` now returns the removed messages; each gets `report_finished(Cancelled)`; then the interrupt gets `report_started`, its `INTERRUPTED_RESPONSE` delivery, `report_finished(Replied)` |
| queue-full rejection (`:1257-1260`) | after delivering `DEFERRED_QUEUE_FULL_RESPONSE`: `report_finished(&rejected, Cancelled)` |
| result mapping in `handle_message` (`:2180-2201`) | before returning: `NeedApproval` → `self.channels.note_turn_outcome(message.id, Paused)`; `Interrupted` → `Interrupted`; `Error{..}` → `Failed` |
| `deliver_message_result` (`:1393-1427`), `Ok(None)` arm (`:1412`) | the DEC-1 guard of 8.4: `Continue` instead of `Shutdown` for `gateway` and `unix_socket_repl` |

`ic/src/agent/dispatch.rs`: `cancel_lane(&mut self, lane) -> Vec<IncomingMessage>` (partition out of the `VecDeque`; `dispatch.rs:248-256`); its tests assert `.len()`.

Exactly-once: each supervised message finishes through exactly one arm; priority controls through their inline path; cancelled queued prompts through the cancel-lane loop; messages drained into another V1 turn (`thread_ops.rs:420-438`) already finished with their own queued reply. Deferred messages dropped at process exit get nothing.

#### 8.2.3 V2 sites (`ic/src/bridge/router/outcome.rs`, `ic/src/bridge/router.rs`)

- `TurnCompletion::complete` (`outcome.rs:252-304`): before the final match, `match &outcome { Stopped => note(Interrupted), Failed{..} | MaxIterations => note(Failed), _ => {} }` via `self.delivery.channels.note_turn_outcome(self.message.id, …)`.
- `insert_and_notify_pending_gate` (`router.rs:280-430`): after the prompt is sent, for every resume kind (`Approval`, `Authentication`, `External`), `channels.note_turn_outcome(message.id, Paused)`.
- `await_thread_outcome` (`outcome.rs:71-97`): in the detach branch, before spawning `deliver_late`, `turn.delivery.channels.note_turn_outcome(message.id, Detached)`; the agent loop then reports `Finished(Detached)`.
- `deliver_late` (`outcome.rs:168-207`): after its delivery, `self.delivery.channels.message_lifecycle(&self.message, MessageLifecycle::Finished(outcome))` where `outcome` is `Replied` for `Ok(Some(non-empty))`, `NoReply` for `Ok(_)`, `Failed` for `Err` (hints set by the late `complete()` apply). The late watcher polls at 100 ms, so its finish cannot precede the agent loop's `Detached` report in practice; if it ever did, the client ignores a `detached` for a sealed turn (3.5).

#### 8.2.4 Channel mappings

- Gateway (`GatewayChannel::message_lifecycle`, M2a): only when the message metadata carries a G1 `message_id` and `msg.thread_id` is set; `Started` → `SseEvent::TurnStarted{thread_id}`, `Finished(o)` → `SseEvent::TurnFinished{thread_id, outcome: o}`, both via `sse.broadcast_correlated(Some(&msg.user_id), &msg.metadata, event)`. Internal messages without G1 metadata (job monitor, routines) emit nothing.
- Socket (M3 for protocol 1, M4 for REPLv3): `TurnStarted` / `TurnFinished` frames for messages with `repl_msg_id`; protocol 1 sessions get the compatibility replies of 8.5.5.

### 8.3 Gateway (M2a; client tokens M2b)

#### 8.3.1 Correlation (G1)

- `ic/src/channels/web/types.rs`: `SendMessageRequest` (`:18`) and `ApprovalRequest` (`:168`) gain `#[serde(default)] pub client_turn_id: Option<String>`. NEW `SseEvent::TurnStarted { thread_id: String }` (`#[serde(rename = "turn_started")]`) and `SseEvent::TurnFinished { thread_id: String, outcome: lunarwing_common::TurnOutcome }` (`#[serde(rename = "turn_finished")]`), with arms in the event-name match (`sse.rs:202-233`) and `WsServerMessage::from_sse_event` (`types.rs:1500`).
- `ic/src/channels/web/server.rs`:
  - `fn validate_client_turn_id(id: Option<&str>) -> Result<Option<&str>, (StatusCode, String)>` using `lunarwing_common::client_caps::is_valid_client_id`; 400 `invalid client_turn_id`.
  - `pub(crate) fn gateway_message_metadata(user_id: &str, thread_id: &str, message_id: Uuid, client_turn_id: Option<&str>) -> serde_json::Value` → `{"user_id","thread_id","message_id", "client_turn_id"?}`.
  - `chat_send_handler`: replace the metadata block (`:1409-1413`) with the helper (`message_id = msg.id`).
  - `chat_approval_handler`: after `with_thread(&thread_id)` (`:1528`), `msg.with_metadata(gateway_message_metadata(&user.user_id, &thread_id, msg.id, client_id))`.
  - Credential continuations (F1): `AuthTokenRequest` and `AuthCancelRequest` (`types.rs:1423-1437`) gain `#[serde(default)] pub client_turn_id: Option<String>`, validated with `validate_client_turn_id`. `queue_engine_auth_resume` (`:1780-1834`) gains a `client_turn_id: Option<&str>` parameter and replaces its metadata block (`:1799-1804`) with `gateway_message_metadata(user_id, thread_id, message.id, client_turn_id)`, so the resumed V2 turn is correlated to the owning turn and gets lifecycle events. `chat_auth_cancel_handler` validates the field but emits nothing new (3.6.4).
- `ic/src/channels/web/sse.rs`:
  - `pub struct Correlation { pub message_id: String, pub client_turn_id: Option<String> }` with `pub fn from_metadata(meta: &serde_json::Value) -> Option<Self>` (requires `message_id` to parse as a UUID; re-validates `client_turn_id`).
  - `ScopedEvent` (`:27-30`) gains `correlation: Option<Correlation>`; `broadcast`/`broadcast_for_user` set `None` (the only two constructors are in this file).
  - NEW `pub fn broadcast_correlated(&self, user_id: Option<&str>, metadata: &serde_json::Value, event: impl Into<SseEvent>)`.
  - NEW `pub fn subscribe_until(&self, user_id: Option<String>, until: impl Future<Output = ()> + Send + 'static) -> Option<impl Stream>`: the stream ends when `until` resolves (used by M2b's client tokens; generic, tested in M2a).
  - `fn sse_payload(event: &SseEvent, corr: Option<&Correlation>) -> Result<String, serde_json::Error>`: uncorrelated events keep `serde_json::to_string`; correlated events go through `to_value`, insert `message_id` and `client_turn_id` at the top level, then `to_string`.
  - `subscribe()` keeps `(event, correlation)` through the user filter; `subscribe_raw` keeps its signature; NEW `subscribe_raw_correlated` (tests).
- `ic/src/channels/web/mod.rs`: `send_status` → `broadcast_correlated(metadata user_id, metadata, event)`; `respond` → `broadcast_correlated(Some(&msg.user_id), &msg.metadata, …)`; `broadcast` (proactive) unchanged. NEW `pub(crate) fn status_sse_event(status: StatusUpdate, thread_id: Option<String>) -> SseEvent` extracted from the inline match (`mod.rs:457-575`), a pure refactor reused by REPLv3.
- `ic/src/bridge/router.rs`: `deliver_thread_event` (`:4122-4141`) and the `GateRequired` broadcast (`:346-363`) use `sse.broadcast_correlated(Some(&message.user_id), &message.metadata, …)`.
- WebSocket clients do not receive correlation fields (the TUI does not use WS).

#### 8.3.2 Lifecycle events

As 8.2.4. Order guarantee: `respond()` broadcasts synchronously into the same broadcast channel before `report_finished` runs on the same task, so every subscriber sees `response` before `turn_finished`.

#### 8.3.3 Capabilities (G7)

- `server.rs`: `.route("/api/chat/capabilities", get(chat_capabilities_handler))` beside the chat routes (`:465-474`); in M2a the handler takes `State` and `AuthenticatedUser` and reports `credential: "master"`; M2b adds `Option<Extension<ClientCredential>>` (axum 0.8 optional extractor). Returns `Json<ChatCapabilitiesResponse>` (4.2.2).
- `types.rs`: `pub struct ChatCapabilitiesResponse { pub api_version: u32, pub server_version: &'static str, pub engine: &'static str, pub features: Vec<&'static str>, pub limits: lunarwing_common::WireLimits, pub voice: VoiceCapabilities, pub credential: &'static str }` and `pub struct VoiceCapabilities { pub provider: Option<&'static str>, pub stt: bool, pub tts: bool }`. Rate-limit constants are hoisted to `pub(crate) const CHAT_RATE_LIMIT: (u32, u64) = (30, 60)` and `VOICE_RATE_LIMIT = (10, 60)` in `mod.rs` and used at `mod.rs:125`, `:139`, `:175`, `:189`.
- `voice.rs`: `VoiceStatusResponse` (`:349`) gains `stt`, `tts`, `provider: Option<&'static str>`, `rate_limit: VoiceRateLimit { requests: u32, window_secs: u64 }`.

#### 8.3.4 Image validation (G4, G5)

`server.rs`: `images_to_attachments` (`:1318-1352`) and `mime_to_ext` (`:1357-1365`) are replaced by a call to `image_mime::decode_images(&inputs, GATEWAY_IMAGE_LIMITS, "web-image")` and the 413/422 mapping of 6.2; `ws.rs:252` uses the same call and sends `WsServerMessage::Error` on rejection. Contract change: an invalid image now fails the request instead of being dropped silently; the web UI's own caps (5 images of 5 MB, `static/app.js:1059-1060`) keep valid uploads working.

#### 8.3.5 Client tokens (G6, M2b)

NEW `ic/src/channels/web/client_tokens.rs`:

```rust
pub const DEFAULT_CLIENT_TOKENS_FILE: &str = "gateway-client-tokens.json";
const RELOAD_INTERVAL: Duration = Duration::from_secs(2);
const MAX_CLIENT_TOKENS: usize = 32;
const MAX_FILE_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct ClientTokensFile { version: u32, tokens: Vec<ClientTokenEntry> }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct ClientTokenEntry { label: String, sha256: String,
    #[serde(default)] created_at: Option<String>, #[serde(default)] expires_at: Option<chrono::DateTime<chrono::Utc>> }

/// Request extension inserted by the auth middleware for a client-token request.
#[derive(Clone)]                 // Debug is hand-written and prints the label only
pub struct ClientCredential {
    pub label: String,
    digest: [u8; 32],
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    store: Arc<ClientTokenStore>,
}
pub struct ClientTokenStore { /* path, owner identity,
    entries: RwLock<Option<Arc<Vec<Entry { digest: [u8; 32], label: String, expires_at: Option<DateTime<Utc>> }>>>> (None = failed closed),
    fingerprint (len, mtime_ns, ino), changes: tokio::sync::watch::Sender<u64> */ }
#[derive(Debug, thiserror::Error)]
pub enum ClientTokenError { Io(std::io::Error), NotRegularFile, WrongOwner, InsecureMode(u32), TooLarge,
    OutsideProtectedDir(PathBuf), Parse(serde_json::Error), UnsupportedVersion(u32), InvalidEntry(String), TooMany }

/// The only entry point main.rs calls (F13). Resolves `GATEWAY_CLIENT_TOKENS_FILE` with
/// `crate::config::helpers::optional_env` (unset -> `crate::bootstrap::lunarwing_base_dir().join(DEFAULT_CLIENT_TOKENS_FILE)`;
/// `none` -> disabled), applies the path policy, builds the owner `UserIdentity`, loads the file (a failure
/// fails closed and is logged here), and returns `None` when client tokens are disabled.
pub fn store_from_env(owner_id: &str) -> Option<Arc<ClientTokenStore>>;
fn configured_path() -> Result<Option<PathBuf>, ClientTokenError>;
impl ClientTokenStore {
    pub fn load(path: PathBuf, owner: UserIdentity) -> Arc<Self>;   // initial synchronous load; failures fail closed
    pub fn authenticate(&self, candidate: &str) -> Option<(UserIdentity, ClientCredential)>;
    pub fn reload_if_changed(&self) -> Result<bool, ClientTokenError>;
    pub fn spawn_reloader(self: &Arc<Self>);                         // tokio interval, RELOAD_INTERVAL
    /// True while the store is loaded (not failed closed) and holds an unexpired entry with the same
    /// label and the same digest (constant-time compare).
    pub fn is_active(&self, cred: &ClientCredential) -> bool;
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64>;
}
/// Resolves when `cred` stops being active: a reload that removes or rotates its entry or fails, or its expiry (SEC-05).
pub async fn credential_revoked(cred: ClientCredential);
```

Rules:
- Path policy (SEC-10): the configured file's canonical parent must be at or inside an entry of `crate::sandbox::mount_guard::resolve_protected_paths()` (`ic/src/sandbox/mount_guard.rs:80`), checked at load and on every reload; otherwise `OutsideProtectedDir` and no client tokens. The default `<base>/gateway-client-tokens.json` passes on single-node installs (`<base>` is protected, `mount_guard.rs:178-181`) and on tenants (`state/`, `06-env-generation.sh:1624`). Protected directories are refused as sandbox mount roots and shadowed under any mounted ancestor (`mount_guard.rs:309-359`, `:378-392`), so a sandboxed command with a writable workdir cannot plant a token file the daemon reads.
- File checks via `symlink_metadata`: regular file, owner `nix::unistd::geteuid()`, `mode & 0o077 == 0`, ≤ 64 KiB, `version == 1`, ≤ 32 entries, labels `^[a-z0-9][a-z0-9:._-]{0,63}$`, `sha256` 64 lowercase hex. Any failure fails closed (`entries = None`, no client tokens) with one `warn!` per change that never logs content. A missing file means no client tokens (debug log).
- `authenticate` hashes the candidate once and compares it with every unexpired entry using `subtle::ConstantTimeEq` without early exit (as `MultiAuthState::authenticate`, `auth.rs:86-95`); the `ClientCredential` it returns carries the matched entry's digest and `expires_at`.
- Reloader: every reload attempt that changes the entry set, or fails, bumps the `watch` counter so that open streams re-check (SEC-05). While the store is failed closed, `is_active` is false for every credential.
- `credential_revoked`:

```rust
pub async fn credential_revoked(cred: ClientCredential) {
    let mut changes = cred.store.subscribe_changes();
    loop {
        let until_expiry = cred.expires_at.map(|at| (at - chrono::Utc::now()).to_std().unwrap_or_default());
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() || !cred.store.is_active(&cred) { return; }
            }
            () = sleep_or_pending(until_expiry) => return,   // tokio::time::sleep(d), or std::future::pending() for None
        }
    }
}
```

- `auth.rs`: `CombinedAuthState` (`:189-192`) gains `pub client_tokens: Option<Arc<ClientTokenStore>>` (the `From` impl and the literal at `mod.rs:98` set `None`). Middleware order: (1) master token from the header (unchanged); (2) a client token from the header: if `!client_token_route_allowed(method, path)` → 403 `Client token not permitted for this route`, else insert `UserIdentity` (owner) and `ClientCredential`, `debug!(credential = %label, …)`; (3) query fallback for the master token only (unchanged).

```rust
fn client_token_route_allowed(method: &Method, path: &str) -> bool {
    (path.starts_with("/api/chat/") && path != "/api/chat/ws")
        || path.starts_with("/api/voice/")
        || (method == Method::GET && path == "/api/gateway/status")
}
```

- `web/mod.rs`: `pub mod client_tokens;`; `GatewayChannel::with_client_tokens(self, store: Arc<ClientTokenStore>) -> Self`; `start()` (`mod.rs:393-416`) calls `spawn_reloader` when a store is set.
- `server.rs`:
  - `chat_events_handler` (`:1888-1900`) takes `Option<Extension<ClientCredential>>`; when present the stream is `state.sse.subscribe_until(Some(user.user_id), client_tokens::credential_revoked(cred))` (8.3.1), so a revoked, rotated or expired credential loses its stream within about 2 s (a rotation keeps the label and swaps the digest; expiry is a timer, not a file change).
  - `chat_capabilities_handler` takes `Option<Extension<ClientCredential>>` and reports `credential: "client"` when present.
  - `chat_auth_token_handler` (`:1625-1720`, SEC-07) takes `Option<Extension<ClientCredential>>`; when present it requires a live authentication request for `req.extension_name`: `pending.is_some()` from `requested_engine_auth_gate` (`:1739-1777`), or NEW `async fn session_has_pending_auth(session_manager: Option<&Arc<SessionManager>>, user_id: &str, extension_name: &str) -> bool` next to `clear_session_auth_mode` (`:1876`), true when any thread of the owner's session has `pending_auth` with that `extension_name` and `!is_expired()` (`ic/src/agent/session.rs:149-162`, `:231`). Otherwise 403 `Client token may only answer a pending authentication request`. The master token path is unchanged.
- `main.rs` (orchestration, after `GatewayChannel::new` at `:814`): `if let Some(store) = client_tokens::store_from_env(&config.owner_id) { gw = gw.with_client_tokens(store); }`.
- On tenants `LUNARWING_BASE_DIR` is the tenant state dir (`ic/scripts/mt-admin-lib/06-env-generation.sh:1437`), so the default file is `/home/<t>/lunarwing/state/gateway-client-tokens.json`, inside `SANDBOX_PROTECTED_PATHS` (`06-env-generation.sh:1624`); no env re-render is needed. The `_FILE` suffix keeps the log scrubber from treating the value as a secret.
- Client tokens share the owner's rate-limit buckets and approval authority; they narrow only the HTTP surface (no settings, secrets, extensions, logs, `/v1/*`, WS or project files), and they can write an extension credential only while the agent is waiting for that credential.

#### 8.3.6 Engine V2 emits `ImageGenerated` (G3)

- `ic/src/tools/builtin/image_gen.rs`: `pub(crate) struct GeneratedImage { pub data_url: String, pub path: Option<String> }`, `pub(crate) const MAX_GENERATED_IMAGE_DATA_URL_BYTES: usize = 16 * 1024 * 1024`, `pub(crate) fn parse_image_sentinel(tool_name: &str, output: &str) -> Option<GeneratedImage>`: accepts only `image_generate`/`image_edit` (after `-`→`_`), JSON with `type == "image_generated"`, `data` starting with `data:image/{png|jpeg|webp|gif};base64,` (lowercase compare), length ≤ cap, optional string `path`. The sentinel is built at `image_gen.rs:168-177`.
- `ic/src/agent/dispatcher.rs:929-966`: the inline parse becomes `parse_image_sentinel(&tc.name, output)` (behaviour preserved plus the MIME and size checks).
- NEW `ic/src/bridge/generated_images.rs`: `GeneratedImageStash { inner: std::sync::Mutex<VecDeque<StashedImage{thread_id, call_id, at, image}>> }` with `put`/`take`, `MAX_STASHED_IMAGES = 8`, `MAX_STASHED_BYTES = 32 MiB`, `STASH_TTL = 600 s`; sweeps expired entries and evicts oldest over budget; never holds the lock across `.await`.
- `ic/src/bridge/effect_adapter.rs`: field `generated_images: GeneratedImageStash` on `EffectBridgeAdapter`; in `execute_action_internal`'s `Ok(output)` arm **before** `sanitize_tool_output` (`:1557-1559`) call `stash_generated_image(lookup_name, &output, context)` (only when `context.source_channel.is_some()` and `context.current_call_id` is `Some`); NEW `pub(crate) fn take_generated_image(&self, thread_id, call_id: &str) -> Option<GeneratedImage>`. The LLM-visible output is unchanged.
- `ic/src/bridge/router.rs`: `forward_event_to_channel` (`:4422`) gains `generated: Option<GeneratedImage>`; in the `ActionExecuted` arm (`:4440-4488`), after `ToolStarted`, `Some(image)` sends `StatusUpdate::ImageGenerated{data_url, path}` instead of `ToolResult` (V1 parity). `deliver_thread_event` passes `state.effect_adapter.take_generated_image(event.thread_id, call_id)` for `ActionExecuted` events (`call_id` is on the event, `crates/lunarwing_engine/src/types/event.rs:337-348`).
- Any V2 channel receives it through `Channel::send_status` with the correlated metadata; REPLv3 needs nothing more.

#### 8.3.7 Voice exposure for REPLv3 (prepared in M2a, used in M4)

`ic/src/channels/web/voice.rs`:
- `pub(crate) const AUDIO_FORMATS` (was private, `:56-58`).
- `pub(crate) fn validate_stt_input(audio_b64: &str, format: &str, max_b64_len: usize) -> Result<(), SttInputError>` with `SttInputError::{Empty, TooLarge, BadBase64, BadFormat}`; `validate_transcribe_request` (`:618-648`) calls it.
- `pub(crate) struct SttTranscript { pub(crate) text: String, pub(crate) language: String, pub(crate) model: String }` and `pub(crate) enum SttFailure { RateLimited, Busy, Timeout, Rejected, Unavailable }`.
- `impl VoiceApi { pub(crate) async fn transcribe_b64(&self, audio_b64: &str, format: &str) -> Result<SttTranscript, SttFailure> }` sends `language: "auto"` and the configured model through the existing private `VoiceProxyClient::transcribe` (`voice.rs:147-155`) and maps errors with a private `fn stt_failure(error: &VoiceProxyError) -> SttFailure`: sidecar 429 → `RateLimited`; 503 → `Busy`; 504 or request timeout → `Timeout`; 400/422 → `Rejected`; 401, other statuses, connection errors, invalid responses → `Unavailable`.
- `VoiceProxyError` and `SidecarTranscribeResponse` stay private (`voice.rs:61-69`, `:369-378`), so no private type appears in a `pub(crate)` signature (F9: rustc's warn-by-default `private_interfaces` lint would fail `clippy -D warnings`). `voice_transcribe_handler` keeps its own call path and `map_proxy_error` (`:550-616`).

### 8.4 `/quit`, `/exit`, `/shutdown` refusal (M2a)

Path today: the parser maps the three commands to `Submission::Quit` (`ic/src/agent/submission.rs:125-127`); V2 routing falls through (`agent_loop.rs:1851`); `Submission::Quit => return Ok(None)` (`:2151`); `deliver_message_result` maps `Ok(None)` to `Shutdown` (`:1412`); the loop breaks (`:1217-1219`) and `main.rs` shuts the daemon down. The arm is the choke point for the commands and has `message.channel` in scope (`/restart` is gated the same way, `ic/src/agent/commands.rs:574-588`). Two other producers return `Ok(None)` (SEC-06): the silent-reply check on every V1 response (`agent_loop.rs:2181-2186`, `is_silent_reply` accepts `NO_REPLY` with up to 4 trailing punctuation or space characters, `ic/src/llm/reasoning.rs:17`, `:200-208`), which applies to the socket always and to V1 gateways, and V2 `Completed{response: None}` (`ic/src/bridge/router.rs:155`, returned by `TurnCompletion::complete`, `ic/src/bridge/router/outcome.rs:286-303`). Both are model output, so injected content can stop the daemon from either TUI channel.

```rust
// ic/src/agent/commands.rs (NEW, near the restart gate)
/// Channels whose clients attach to a running service: `/quit`, `/exit`, `/shutdown`
/// must not stop the daemon from them (TUI design 8.4).
const QUIT_REFUSED_CHANNELS: &[&str] = &[crate::channels::GATEWAY_CHANNEL, crate::channels::UNIX_SOCKET_REPL_CHANNEL];
pub(super) const QUIT_REFUSED_MESSAGE: &str =
    "/quit, /exit and /shutdown cannot stop the LunarWing daemon from this channel. \
     Close the client instead; operators use the service manager (lunarwing-mt-admin.sh stop-tenant).";
pub(super) fn quit_refusal(channel: &str) -> Option<&'static str> {
    QUIT_REFUSED_CHANNELS.iter().any(|c| c.eq_ignore_ascii_case(channel)).then_some(QUIT_REFUSED_MESSAGE)
}

// ic/src/agent/agent_loop.rs:2151, replacing `Submission::Quit => return Ok(None),`
Submission::Quit => match super::commands::quit_refusal(&message.channel) {
    Some(reason) => {
        tracing::warn!(channel = %message.channel, "Refused /quit from a channel that may not stop the daemon");
        Ok(SubmissionResult::error(reason))          // delivered as "Error: ..." (agent_loop.rs:2192); lifecycle Failed
    }
    None => return Ok(None),
},
```

Decision DEC-1 (recommended; SEC-06): without touching either producer, the shutdown decision itself refuses the two client channels:

```rust
// ic/src/agent/agent_loop.rs, deliver_message_result (:1393-1427), replacing `Ok(None) => MessageLoopControl::Shutdown,`
Ok(None) => none_result_control(&message.channel),

/// `Ok(None)` stops the loop except on channels whose clients attach to a running service (TUI design 8.4).
fn none_result_control(channel: &str) -> MessageLoopControl {
    if super::commands::quit_refusal(channel).is_some() {
        tracing::warn!(channel = %channel, "refused shutdown requested through a client channel");
        MessageLoopControl::Continue
    } else {
        MessageLoopControl::Shutdown
    }
}
```

The lifecycle hook already reports `Ok(None)` as `NoReply` (8.2.2), so a silent reply finishes as `no_reply` on these channels. The two tracked bugs keep their effect on every other channel and stay filed separately (DEC-6). If Christopher declines DEC-1, the guard is not built and 10.1 and 10.4 say instead: "model output can still stop the daemon over both TUI channels (a V1 reply that is exactly `NO_REPLY`, or V2 `Completed{response:None}`); tracked separately".

The in-process REPL (channel `"repl"`, `ic/src/channels/repl.rs:490-492`) and all other channels keep today's behaviour. REPLv3 additionally answers `Error{forbidden_command}` from the channel without forwarding (8.6.1). Bare `exit`/`quit` remain intercepted by both Rust clients (`cli/repl.rs:117`, `unix-socket-client/src/lib.rs:102`).

### 8.5 Socket hardening on protocol 1 (M3)

#### 8.5.1 Module layout

`ic/src/channels/unix_socket_repl.rs` (359 lines) becomes a directory module; `mod unix_socket_repl;` in `channels/mod.rs` is unchanged. The uncompiled orphan `ic/src/unix_socket_repl.rs` is deleted (open question A.4 recommends yes).

| File | M3 contents | M4 additions |
|---|---|---|
| `mod.rs` | `UnixSocketReplChannel` (same `new(socket_path, max_connections, agent_name, owner_id)` signature, `main.rs:501-506`), `Channel` impl delegating to the modules, `#[cfg(test)] with_limits`, `with_expected_uid` | `with_voice(Option<Arc<VoiceApi>>)` |
| `listener.rs` | parent-dir policy, bind-first + liveness probe, chmod 0600, `SocketIdentity`, `peer_allowed`, `accept_loop`, `AcceptBackoff` | |
| `conn.rs` | `serve_connection`, `read_handshake`, `negotiate` (v1 only), `open_session`, `read_frame_line`, `read_loop`, `dispatch` | v3 negotiation, idle timeout, v3 dispatch |
| `writer.rs` | `Outbound`, `FrameClass`, `Delivery`, `writer_task` | stream-gap notice |
| `registry.rs` | `Registry` (sessions), `Session`, `SlotGuard` | scopes, `TurnTable`, pending approvals, in-flight budget, undelivered stash |
| `outbound.rs` | v1 mapping: reply routing, approval prompt as `Response`, lifecycle compatibility replies | v3 frames, `Status` via `status_sse_event`, `TurnFinished` |
| `inbound.rs` | (v1 messages are forwarded by `conn.rs`) | Message, Interrupt, Steer, Approval → `IncomingMessage`; attachments via `decode_images`; quit pre-check |
| `transcribe.rs` | | `TranscribeService` (8.6.3) |
| `tests.rs` | 11.1 socket tests | REPLv3 tests |

Errors: `ReplSocketError` (startup, thiserror), converted into `ChannelError::StartupFailed{name:"unix_socket_repl", reason}`; `start_all` logs it and keeps other channels running (`manager.rs:215-235`). A `ConnCtx` struct carries shared state so no function takes 7 or more arguments.

#### 8.5.2 WI#458 item 1: explicit permissions

```rust
// listener.rs
fn prepare_parent_dir(path: &Path) -> Result<(), ReplSocketError>;   // missing: create 0700 (DirBuilder.mode + set_permissions, umask-proof)
fn validate_parent(parent: &Path, meta: &std::fs::Metadata) -> Result<(), ReplSocketError>;
// existing parent: must be a directory owned by geteuid with mode & 0o022 == 0, else InsecureParent (SEC-11); never chmod
fn set_mode_0600(path: &Path) -> Result<(), ReplSocketError>;        // after bind
```

Existing directories are never chmodded because they are shared: `$XDG_RUNTIME_DIR` (0700) and the multi-tenant `run/` (0750 on OpenRC via `checkpath`, `ic/scripts/mt-admin-lib/08-units-openrc.sh:1122`, also holding `ssh-agent/`). The bind-to-chmod window is harmless because every connection passes the uid gate and MT units run with `UMask=0077` (`07-units-systemd.sh:1054`, `08-units-openrc.sh:916`). The SSH-agent socket precedent (`ic/src/bridge/ssh_agent.rs:105-153`, 0755/0666) is deliberately not copied: that socket must be reachable by worker uids; this one must not.

The strict owner and mode rule (SEC-11) holds for every layout in use: `$XDG_RUNTIME_DIR` is 0700 and user-owned; the OpenRC tenant `run/` is 0750 and tenant-owned (`08-units-openrc.sh:1122`); a daemon-created `run/` is 0700 under `UMask=0077`; a dev harness `run/` is 0755 at worst. In a group- or world-writable parent another uid could pre-bind or replace the path, and the legacy clients would talk to it. A socket configured in a shared directory such as `/tmp` is therefore refused at start (today it works); the error names the directory and its mode.

#### 8.5.3 WI#458 item 2: same-uid `SO_PEERCRED` gate

`peer_allowed(&UnixStream, expected_uid) -> Result<(), PeerRejected>` runs on every accepted stream before any byte is read, using `UnixStream::peer_cred()` (`ic/vendor/tokio/src/net/unix/stream.rs:981`); `expected_uid` defaults to `nix::unistd::geteuid()`. A rejected connection is closed and logged at warn with the peer uid and pid. Limitation (documented): credentials are reported as mapped into the daemon's user namespace, so a rootless-podman sandbox process maps to the daemon uid and passes; path placement and 8.5.7 are the controls for that.

#### 8.5.4 WI#458 item 3: bind first, probe before unlinking, under a lock (fixes WI#385 path theft)

```rust
pub(super) struct BoundSocket { listener: UnixListener, identity: SocketIdentity, _lock: std::fs::File }

pub(super) async fn bind_socket(path: &Path) -> Result<BoundSocket, ReplSocketError> {
    prepare_parent_dir(path)?;                       // 8.5.2: owner and mode checked
    let lock = acquire_path_lock(path)?;             // <path>.lock, held for the listener's lifetime
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => match probe_existing(path).await? {
            Existing::Live => return Err(ReplSocketError::AlreadyServing(path.into())),
            Existing::Foreign { uid } => return Err(ReplSocketError::ForeignOwner { path: path.into(), uid }),
            Existing::NotSocket => return Err(ReplSocketError::NotASocket(path.into())),
            Existing::Stale => { std::fs::remove_file(path)?; UnixListener::bind(path)? }   // only under the lock
        },
        Err(e) => return Err(e.into()),
    };
    set_mode_0600(path)?;
    Ok(BoundSocket { identity: SocketIdentity::of(path)?, listener, _lock: lock })   // (dev, ino) from symlink_metadata
}

/// Opens `<path>.lock` with O_CREAT | O_NOFOLLOW | O_CLOEXEC (`OpenOptionsExt::custom_flags`, `libc`), mode 0600,
/// and takes `std::fs::File::try_lock()` (stable since Rust 1.89, MSRV 1.96.1, no new crate; flock(2) on Linux,
/// dropped by the kernel when the process dies). `Err(TryLockError::WouldBlock)` -> `AlreadyServing`.
fn acquire_path_lock(path: &Path) -> Result<std::fs::File, ReplSocketError>;
```

- The lock serializes starts (SEC-11): two daemons racing on one stale path can no longer both classify it `Stale` and unlink each other's freshly bound socket. The stale unlink and the shutdown unlink happen only while the lock is held. The lock file is never removed; an orphaned one is harmless because the kernel drops the lock with the process.
- `probe_existing`: `symlink_metadata` first; a symlink or non-socket is `NotSocket` (never followed); a socket inode whose owner is not `geteuid()` is `Foreign{uid}` (refused, never probed, never unlinked); otherwise a 500 ms connect: success or timeout → `Live`; `ECONNREFUSED`/`ENOENT` → `Stale`; other errors (EACCES) → error, path untouched.
- `set_mode_0600(path)` runs right after the bind, in a parent only this uid can write (8.5.2), while the lock is held, so the path cannot be swapped in between.
- Not serving instead of failing (F8): `AlreadyServing`, `ForeignOwner`, `NotASocket` and `InsecureParent` cannot be fixed by restarting, so `start()` logs them once at `error!`, returns `Ok` with a stream that never yields (`futures::stream::pending()`), and records `ListenerState::NotServing { reason }`; `health_check()` then returns `Ok(())` and logs one `warn!`. Otherwise the single-node watchdog, which restarts the service whenever any channel is unhealthy (`ic/scripts/lunarwing-watchdog.sh:85-104`, `ic/scripts/lunarwing-watchdog-openrc.sh:85-88`, hourly timer `ic/systemd/lunarwing-watchdog.timer`), would restart the daemon every hour forever, because `health_check_all` marks channels whose start failed as unhealthy (`ic/src/channels/manager.rs:343-363`). Other I/O errors (for example `EACCES` creating the parent) still fail `start()`.
- `shutdown()` cancels the accept loop, sends `Notice{shutdown}` to v3 sessions (M4), and unlinks the path only if `SocketIdentity::matches(path)` and the lock is held. A missing or replaced path (WI#385) found by `health_check()` is logged once at `warn!` with both inodes and reported as `Ok(())`, for the same watchdog reason.

#### 8.5.5 Reader, writer and the protocol 1 compatibility behaviours

- Accept loop: `tokio::select!` on a `CancellationToken` and `listener.accept()`; peer gate; at most 8 pending handshakes (`Semaphore::try_acquire_owned`, excess connections closed); accept errors back off 10 ms doubling to 1 s. Today's loop never exits and survives `hot_add` (`manager.rs:163-176`).
- Bounded reads (D2): `read_frame_line(reader, buf, max)` reads with `(&mut *reader).take(max + 1).read_until(b'\n', buf)` (tokio `Take` implements `AsyncBufRead`, `ic/vendor/tokio/src/io/util/take.rs:107`), strips `\n` and an optional `\r`, and returns `Line | Eof | TooLong`. Handshake: 16 KiB within 10 s (D8). After the handshake: 16 MiB + 64 KiB. Buffers over 1 MiB are replaced after each frame.
- Handshake logging (D9): a non-`Connect` first frame logs only `frame.kind()`; `version`/`client_info` are logged truncated to 64 characters with control characters removed. No frame payload is ever logged; hand-written `Debug` in `repl_wire` (4.3.2).
- Writer task and bounded queue (D1, part 1): `respond`, `send_status` and `message_lifecycle` only call `Outbound::offer(frame, class) -> Delivery` (never awaits). Per connection: `mpsc` of 1024 frames and a 32 MiB byte budget; a writer task owns the `OwnedWriteHalf` and does `timeout(15 s, write_all)`. Status frames are shed when more than 2 MiB is queued or the queue is full (the next delivered status is preceded by `stream_gap{missed_events}`; `TurnFinished.dropped_status` reports the count). Control frames are never dropped: overflow or a 15 s write stall cancels the connection's `kill` token, which stops reader and writer, and `respond()` returns `Err(SendFailed{reason:"slow consumer disconnected"})`. Registry state uses `std::sync` locks held only for map lookups; `clippy::await_holding_lock` guards regressions.
- Busy: `Registry::try_reserve(max_connections)` returns a `SlotGuard`; when full, protocol 1 gets today's "Server busy" `Response`.
- Protocol 1 compatibility (D3 for legacy clients; the lockstep `lunarwing repl` reads one frame per send):
  - `send_status(ApprovalNeeded)` → one `Response` with the XMPP-style prompt (4.3.8 G). Parameters longer than 8 KiB are cut at a char boundary and followed by `[... N more bytes not shown; review the full request in the web UI before approving]`, so a lockstep client never approves text it cannot see without being told (SEC-04).
  - Lifecycle `Finished(NoReply)` for a message whose content parsed as an approval keyword with nothing delivered → `Response{"Nothing is waiting for approval."}`; any other `NoReply` with nothing delivered → `Response{"(no reply)"}`; `Paused` → nothing (the prompt was already sent).
  - Replies route by `metadata.unix_session_id`, falling back to `sender_id` (today's behaviour, `unix_socket_repl.rs:315-319`).
  - All other bytes are unchanged; golden fixtures pin them (8.5.6).

#### 8.5.6 Shared wire module and legacy clients

- NEW `ic/crates/lunarwing_common/src/repl_wire.rs` with the complete type set of 4.3.2 (v1 and v3; v3 is unused by the server until M4) and fixtures `ic/crates/lunarwing_common/tests/fixtures/replv3/{v1_frames,client_frames,server_frames}.ndjson`, read in `#[cfg(test)]` with `include_str!` so they run in CI's `rust-test-lib` job (`cargo test --workspace --lib`, `.gitlab/ci/rust.yml:145`).
- `ic/src/cli/repl.rs` and `ic/unix-socket-client/src/lib.rs` keep their private v1 enums (R19); each gains a `#[cfg(test)]` test that serializes its `Connect`, `Message`, `Disconnect`, `Ping`, `Pong` and parses the server `Response` lines exactly as in `v1_frames.ndjson` (`include_str!("../../crates/lunarwing_common/tests/fixtures/replv3/v1_frames.ndjson")` from `ic/src/cli/`, and the matching relative path from the standalone crate).

#### 8.5.7 WI#458 item 4: Unix sockets reachable from the sandbox (`ic/src/sandbox/mount_guard.rs`)

Any Unix socket inside a mounted tree is an IPC path out of the sandbox: `connect()` works through `:ro` bind mounts, and podman's keep-id mapping makes the sandboxed process the host uid (`ic/src/sandbox/cli_runner.rs:118-120`), so it passes the `SO_PEERCRED` gate of 8.5.3. Today `masked_files_for_mount` (`mount_guard.rs:451-498`) ignores socket inodes, and the shell tool accepts any absolute `workdir` (`ic/src/tools/builtin/shell.rs:893-896`) that `validate_mount_dir` does not refuse (`mount_guard.rs:309-359`). A walk alone cannot close this: it stops silently at depth 5 and after 10 000 entries (`:436-438`, `:461-470`, `:486`) and runs only at plan time (`:446-449`). Two mechanisms (SEC-02):

1. **Structural protection of the socket directories**, independent of any walk. `resolve_protected_paths_from(env_raw, base, runtime_dirs: &[PathBuf])` (`:167`) gains the last parameter so tests stay environment-free; `resolve_protected_paths_signalled` (`:86`) fills it from NEW `fn runtime_socket_dirs() -> Vec<PathBuf>`:
   - `$XDG_RUNTIME_DIR` when absolute, and `/run/user/<geteuid>`: the default REPL socket (`ic/src/main.rs:490-499`), the D-Bus session bus, `systemd/private`, `podman/podman.sock`, Wayland, PipeWire, and agent sockets kept there;
   - the parent of `LUNARWING_SOCKET` / `IRONCLAW_SOCKET` when set (the base-dir fallback is already protected);
   - `/tmp/.X11-unix` and `${TMUX_TMPDIR:-/tmp}/tmux-<geteuid>` (an X11 client can read every keystroke; a tmux client can type into the user's terminals).
   They join the protected set, so `validate_mount_dir` refuses them as roots (at or inside) and `masked_dirs_for_mount` (`:378-392`) shadows each of them whole with an empty directory under any mounted ancestor, whether or not it exists yet (WI#209). The proxy relay socket and the mask artifacts also live under `$XDG_RUNTIME_DIR` (`ic/src/sandbox/proxy/relay.rs:103-117`) but are mounted directly as sources (`cli_runner.rs:419-471`), so the shadowing does not touch them.
   `validate_mount_dir` also refuses the canonical roots `/`, `/run` and `/var/run` (NEW `const DENIED_MOUNT_ROOTS: [&str; 3]`): they expose system sockets such as `/run/dbus/system_bus_socket` that no bounded walk can be trusted to reach.
2. **The walk also masks the sockets it reaches.** It becomes `fn scan_mount(root: &Path, skip: &[PathBuf], caps: ScanCaps) -> MaskScan` with `pub struct MaskScan { pub files: Vec<PathBuf>, pub sockets: Vec<PathBuf>, pub truncated: bool }` and `struct ScanCaps { max_depth: usize, max_entries: usize }` (defaults 5 and 10 000; tests pass small caps). `masked_files_for_mount(root, skip) -> Vec<PathBuf>` keeps its signature and its callers (`cli_runner.rs:813`, `mount_guard.rs:296`), returns `files` plus `sockets` (each shadowed by the read-only empty-file bind regardless of name), and logs `warn!(mount = %root.display(), "sandbox mask scan truncated; Unix sockets deeper than 5 levels or after 10 000 entries are not masked")` once per root per process when `truncated`.

```rust
if ft.is_file() {
    if is_protected_file_name(&path) { scan.files.push(path); }
} else if is_unix_socket(&ft) {
    scan.sockets.push(path);
} else if ft.is_dir() {
    if skipped.iter().any(|s| path.starts_with(s)) { continue; }      // shadowed whole (step 1 or protected dirs)
    if depth + 1 >= caps.max_depth { scan.truncated = true; continue; }
    stack.push((path, depth + 1));
}
// and where the entry budget stops the walk (today's `break`s at :461-470): `scan.truncated = true;`
#[cfg(unix)] fn is_unix_socket(ft: &std::fs::FileType) -> bool { use std::os::unix::fs::FileTypeExt; ft.is_socket() }
#[cfg(not(unix))] fn is_unix_socket(_ft: &std::fs::FileType) -> bool { false }
```

   A truncated scan does not refuse the mount. The default sandbox workdir of every tenant is the daemon's working directory `<root>/ic` (`ic/scripts/mt-admin-lib/06-env-generation.sh:1615-1618`), a source tree far beyond both caps, so refusing would fail every default sandboxed command. Sockets in the directories of step 1 are covered regardless; a socket deep inside some other large tree is a documented residual risk (decision DEC-4).
- `tenant_logs_mount_source_from` (`:277`, check at `:296-301`) also rejects a logs directory containing a socket; its message becomes "it contains the credential-bearing file or Unix socket {path}"; the module docs say "credential files and Unix sockets".

#### 8.5.8 `ChannelManager` lock release (D1, part 2)

Every method clones the `Arc<dyn Channel>` with `get_channel` (`manager.rs:397-399`), drops the guard, then awaits: `respond` (`:253-274`), `send_status` (`:280-305`), `broadcast` (`:310-331`); `broadcast_all`, `health_check_all`, `shutdown_all` (`:336-389`) iterate a snapshot taken under a short read lock. Reason: tokio's `RwLock` is write-preferring, so a `hot_add`/`remove` writer queued behind one slow channel blocks every channel's `respond` and `send_status`. A `respond` racing `hot_add` may reach the replaced instance, which then fails or no-ops (preferable to blocking).

#### 8.5.9 Defect checklist

| ID | Fix | Milestone |
|---|---|---|
| D1 lock across writes; manager lock across awaits | writer task + bounded queue; lock release; lifecycle is an enqueue | M3 |
| D2 unbounded reads | `read_frame_line` caps | M3 |
| D3 approval/auth prompts dropped | v1: prompt as `Response` + no-reply notices (M3); v3: `ApprovalRequest`, `AuthRequired`, `TurnFinished` (M4) | M3, M4 |
| D4 size checked after decode; queues hold payloads | base64 lengths checked before decoding; 8 messages / 20 MiB in flight per connection | M4 |
| D5 no chmod, no peer check | 0600, strict parent policy, `SO_PEERCRED`, start lock, foreign-owner refusal | M3 |
| D6 declared MIME trusted | `decode_images` sniffing, sniffed type used | M4 (gateway M2a) |
| D7 attachment ids collide | `repl-image-<uuid>` (gateway `web-image-<uuid>` in M2a) | M4 |
| D8 no handshake timeout, unlimited pre-handshake | 10 s timeout, 8 pending handshakes, accept backoff | M3 |
| D9 payloads in logs | kind-only logs, redacting `Debug` | M3 |
| D10 no capability detection | `Welcome{features, limits, engine}`; v1 refuses attachments loudly | M4 |
| D11 errors look like answers | typed `Error` frames | M4 |
| D12 unbounded filenames | sanitized, ≤ 128 characters | M4 |

### 8.6 REPLv3 channel (M4)

#### 8.6.1 Inbound handling

```rust
// inbound.rs
pub(super) async fn on_message(frame: MessageFrame, sess: &Arc<Session>, ctx: &ConnCtx) -> Flow;
// 1. resolve msg_id (is_valid_client_id; assign s-<uuid32> when absent; duplicate in flight -> bad_frame)
// 2. precheck: per-connection limiter (reuse web::server::RateLimiter semantics), content <= 100000 bytes,
//    attachments require feature `attachments`, non-empty content or attachments
// 3. classify(content) with SubmissionParser::parse (submission.rs:14-208):
//    Quit -> Error{forbidden_command}; ApprovalResponse with nothing pending on the scope -> Error{no_pending_approval};
//    ApprovalResponse with a pending entry -> forwarded with repl_msg_id = pending.msg_id (ApprovalResolved follows);
//    ExecApproval JSON typed by hand -> validated like the Approval frame; scope.auth_pending skips these checks once
// 4. decode_images(&inputs, SOCKET_IMAGE_LIMITS, "repl-image")
// 5. registry.begin_turn(sess, msg_id, incoming.id, kind, attachment_bytes)  (busy: 8 in flight or 20 MiB)
// 6. forward: queue Ack, then msg_tx.send(incoming).await (backpressure applies to this client only; no lock held)
```

`build_incoming` sets metadata `{"unix_session_id", "repl_msg_id"}` (+ `"repl_ctl_id"` for approvals) and `with_thread("unix:<token>")`. Controls: `Interrupt` → content `/interrupt`; `Steer` → `/steer <content>` (non-empty, ≤ 100000 bytes). After `Interrupt`, `/clear`, `/new`, `/thread new` or `/undo` on a thread with pending approvals, the channel clears that scope's pending map and emits `ApprovalResolved{cancelled}` (`process_interrupt` interrupts an awaiting thread, `thread_ops.rs:1119-1149`; `process_clear` resets it, `:1239-1258`).

- The keyword precheck stays fail-closed (F7, partly rejected): a bare keyword with no pending entry on the scope gets `Error{no_pending_approval}` and is not forwarded, even when the V1 thread is in fact waiting for an approval the channel never saw. That happens for internal turns on a socket scope (job-monitor messages have `metadata: Null`, `ic/src/agent/job_monitor.rs:107-116`), whose `ApprovalNeeded` cannot be routed (8.6.4). Forwarding the keyword would approve a tool call whose parameters no client displayed. The user instead sees V1's reply "Waiting for approval: <tool> — <description>. Use /interrupt to cancel." (`ic/src/agent/thread_ops.rs:449-461`) and can interrupt.
- `ic/src/agent/submission.rs:17` logs every parsed message with its full text at debug level, which on the socket includes a credential typed after `AuthRequired`. M4 replaces it with `tracing::debug!(content_len = trimmed.len(), "[SubmissionParser::parse] parsing input")` (test `parse_debug_log_omits_content`, using `tracing-test`, a dev-dependency at `ic/Cargo.toml:235`).

#### 8.6.2 Approvals and per-connection threads

- Threads are keyed `(user_id, channel, conversation_scope)` (`ic/src/agent/session_manager.rs:20-25`); `with_thread("unix:<token>")` gives every v3 window its own thread. `maybe_hydrate_thread` only hydrates UUID-shaped scopes (`thread_ops.rs:198-206`), and the `unix:` prefix never parses as a UUID; cross-thread approval by UUID runs only for UUID scopes (`agent_loop.rs:1867-1913`). One window can therefore no longer approve another window's tool, and `/interrupt` cancels only its own lane (`LaneKey` includes the scope, `dispatch.rs:65-90`).
- `send_status(ApprovalNeeded, metadata)` (routed by `unix_session_id`, core, not gated by `status`): `registry.remember_approval(scope, PendingApproval{request_id, msg_id: repl_msg_id, frame})` (max 8 per scope), then queue `ApprovalRequest`. `AuthRequired` → frame + `scope.auth_pending = true`; the next `Message` is forwarded untouched and never logged.
- `Approval{msg_id: ctl, request_id, decision}` → `take_approval(scope, request_id)` or `Error{unknown_request}`; build `Submission::ExecApproval{request_id, approved, always}` JSON (`{"ExecApproval":{"request_id":"<uuid>","approved":true,"always":false}}`, accepted by the parser because it starts with `{`, `submission.rs:168-175`), with `repl_msg_id = pending.msg_id` and `repl_ctl_id = ctl`; `ApprovalResolved` to the scope; `Ack{ctl}`. This is the gateway's form (`chat_approval_handler`), which `process_approval` checks by id and V2 matches with `has_matching_engine_approval(message, Some(request_id))` (`agent_loop.rs:1789-1803`). `always` is clamped by the agent when not offered (`session.rs:200`).
- Stale entries: if the agent answers `""` and the segment finishes `NoReply` without a new prompt, the channel emits `Error{msg_id: ctl, no_pending_approval}` before `TurnFinished`.
- One live connection per thread token (F3): `Registry::bind_scope(token, session)` fails while another live v3 session holds the token, and the handshake answers `Error{thread_in_use, retryable:true, fatal:true}`. A reconnect after the old connection has closed succeeds and gets the replay of 8.6.4.
- In-flight accounting (F2): `TurnTable` releases a message's slot and its attachment bytes when its lifecycle reports `Finished(Paused)` and takes the slot again on the continuation's `Started`, even above the cap; a paused turn never makes the connection `busy`.
- Approval parameters: the `ApprovalRequest` frame carries the full redacted parameters (4.3.6); only an oversize frame sets `parameters_truncated`, and the client then refuses to approve (3.6.2).
- Limits of scoping: v1 sessions keep the shared `(owner, unix_socket_repl, None)` thread (unchanged); v1 and v3 threads never mix; non-gateway messages remain globally serialized; V1 history for non-UUID scopes does not survive a daemon restart (same as today's shared thread).

#### 8.6.3 Transcribe (LunarVoice via `VoiceApi`)

- `main.rs`: build `let voice_api = VoiceApi::from_config(&config.transcription).map(Arc::new);` once before the socket block (`:489`), pass `voice_api.clone()` to `UnixSocketReplChannel::new(...).with_voice(...)` and to the gateway (`gw.with_voice_api`, replacing the inline construction at `:816-821`). One instance, one "voice API enabled" log line.
- `transcribe.rs`:

```rust
pub(super) struct TranscribeService { voice: Arc<VoiceApi>, timeout: Duration /* 150 s */ }
pub(super) fn on_transcribe(frame: TranscribeFrame, sess: &Arc<Session>, ctx: &ConnCtx);
// on the reader task, never awaiting the sidecar:
//   feature `transcribe` negotiated, else Error{feature_not_negotiated};
//   req_id valid; validate_stt_input(audio, format, 16 MiB) -> bad_frame | too_large | unsupported_media;
//   per-connection limiter 10/60 s -> rate_limited; sess.transcribe_busy swap -> busy
// spawned task (cancelled with the connection):
//   timeout(150 s, voice.transcribe_b64(&audio, &format))
//   Ok(r) if r.text.trim().is_empty() -> Error{req_id, no_speech}
//   Ok(r) -> TranscribeResult{req_id, text, elapsed_ms, language: Some(r.language), model: Some(r.model)}
//   Err(e) -> match e.stt_failure() { RateLimited => rate_limited, Busy => busy, Timeout => timeout,
//                                     Rejected => transcribe_failed, Unavailable => unavailable (retryable) }
//   log req_id, byte length and outcome kind only; never the audio or the transcript
```

- Advertised only when `VoiceApi` exists and the client requested it. Message intake continues while a transcription runs; audio never enters the agent loop or the workspace.

#### 8.6.4 Notices, routing and thread resume

- Routing (F7): `respond` and `message_lifecycle` receive the `IncomingMessage` and resolve, in order: `metadata.unix_session_id` if that session is connected; else every v3 session whose scope equals `msg.thread_id` (job notices carry `with_thread(route.thread_id)`, `job_monitor.rs:107-116`; their `sender_id` defaults to the owner id, not a session id, `ic/src/channels/channel.rs:106-128`); else, for control-class frames, a per-scope stash (32 frames, 8 MiB, 15 min TTL) replayed after `Welcome` to the next connection with that token. `send_status` receives only `(StatusUpdate, metadata)` (`channel.rs:512-518`), and `StatusUpdate::ApprovalNeeded` carries no thread id (`channel.rs:372-381`), so status routes by `metadata.unix_session_id` only; an unroutable status, including the approval prompt of an internal turn, is dropped with a metadata-only `warn!` (request id and tool name).
- A reply without `repl_msg_id` reaches v3 sessions as `Notice{kind:"background", level:"info", text}`; v1 sessions keep today's unsolicited `Response` on an exact session match.
- Optional (open question A.9): implement `Channel::broadcast` to send `Notice{kind:"broadcast"}` to v3 sessions (heartbeat, routine and self-repair notifications).

### 8.7 Streaming and Engine V2 for the socket

Decision: stay on V1 for the socket (I9). Evidence: eligibility is a const allowlist (`router.rs:60`); `StreamChunk` comes only from V2 (`router.rs:4585-4593`); parallel dispatch is gateway-only (`dispatch.rs:26-33`), so V2 socket turns would still serialize globally; V2 image delivery is missing until M2a's G3. The protocol is engine-agnostic (`Welcome.engine`, the `stream` feature, `stream_chunk` events in the gateway shape), so a later opt-in is a small change with no wire change: add `"unix_socket_repl"` to `ENGINE_V2_OPT_IN_CHANNELS` behind `ENGINE_V2_CHANNELS`, with integration tests modelled on `ic/tests/engine_v2_channel_delivery.rs` (socket-scoped approval, interrupt, stream chunks). Not scheduled (open question A.7).

### 8.8 Attachment ids

All new attachment ids are `<prefix>-<uuid v4>` from `decode_images()`: `web-image-<uuid>` (gateway, replacing `web-image-{i}` at `server.rs:1341`) and `repl-image-<uuid>` (socket, replacing vibetui4's `repl-attachment-{i}`). The store path is `attachments/<channel>/<sanitized id>/<file>.md` (`ic/src/agent/attachment_store.rs:45-79`); `sanitize_id` keeps `-`, so ids no longer collide. The model-visible attachment block uses the index, not the id (`ic/src/agent/attachments.rs:100-143`), so prompts are unchanged.

---

## 9. Multi-tenant access: `lunarwing-mt-admin.sh tui-access`

### 9.1 Why a new verb

- The desktop user `sun` owns the microphone, clipboard and Hex session (`/run/user/1000`, 0700) but cannot read tenant sockets, env files or tokens: tenant homes are 0700 and daemons run with `UMask=0077` (`ic/scripts/mt-admin-lib/07-units-systemd.sh:978`, `:1054`; `08-units-openrc.sh:916`, `:1127`). A socket TUI run as the tenant loses mic and clipboard access, so tenants are reached through the gateway.
- Only the master token can authenticate today: `GATEWAY_USER_TOKENS` is parsed but never wired into auth (`ic/src/config/channels.rs:66-69`; `GatewayChannel::new` builds a single-token state, `ic/src/channels/web/mod.rs:85-101`), and DB auth is a stub (`ic/src/channels/web/auth.rs:171-185`). The master token also feeds DarkIRC and Brave uploads, the worker fallback and the readiness probe, so rotating it is disruptive and no verb does it.
- `mt-admin tokens [name]` prints master tokens in clear (`ic/scripts/mt-admin-lib/04-tenant-lifecycle.sh:2258-2288`); `tui-access` must not imitate it.

### 9.2 Command

```
tui-access <tenant> --for-user <user> [--url-host <host>] [--expires-in-days <n>] [--use-master-token] [--no-verify]
tui-access <tenant> --for-user <user> --revoke
tui-access <tenant> --list
```

- Root required; the tenant must exist in the registry; `--for-user` is mandatory for grant and revoke (no `$SUDO_USER` default). Output never contains a token or a digest; `--list` prints only validated labels and timestamps (SEC-13).
- Default mode mints a dedicated client token (needs a daemon with M2b client tokens). `--use-master-token` copies `GATEWAY_AUTH_TOKEN` instead (works with today's daemons; prints a warning; the file records `token_kind:"master"`). This is the M1a path until tenants run an M2b binary.

```
tui-access: granted tenant 'derpy' to user 'sun'
  url:     http://127.0.0.1:10000
  config:  /home/sun/.config/wingtip/tenants/derpy.json (0600, owner sun)
  token:   dedicated client token 'tui:sun' (SHA-256 registered tenant-side; plaintext only in the config file)
  owner:   port 10000 is held by uid 1010 (derpy); recorded as tenant_uid
  verify:  ok (GET /api/gateway/status -> 200)
```

| Exit | Meaning |
|---|---|
| 0 | done, including "gateway not reachable now" and "port held by another uid" (warned; authenticated probe skipped; files kept) |
| 1 | refused or failed (`die`) |
| 2 | files written, but the running daemon rejected the new token after 4 probes over about 8 s (binary predates client tokens); the message suggests `upgrade-tenant <t>` or `--use-master-token` |

Dispatcher in `ic/scripts/lunarwing-mt-admin.sh` `main()` next to `tokens)`; usage lines in `usage()`; a source line for the new module after `15-worker-egress.sh`:

```bash
    tui-access)
      require_root
      [[ -n "${1:-}" ]] || die "usage: tui-access <name> --for-user <user> [--revoke] | --list"
      tui_access_command "$@"
      ;;
```

### 9.3 Algorithm (NEW `ic/scripts/mt-admin-lib/16-tui-access.sh`)

Every step reuses an existing helper (all verified to exist):

| Step | Helper |
|---|---|
| tenant name and registry | `sanitize_name` (`00-common.sh:342`), `ports_registry_require_readonly` (`03-ports-registry.sh:72`), `tenant_exists_in_registry` (`03-ports-registry.sh:1271`) |
| serialize against lifecycle verbs | `with_tenant_lifecycle_lock <name> <fn…>` (`00-common.sh:215`) |
| gateway port | `ports_get "$name" gateway` (`03-ports-registry.sh:1199`) |
| tenant env values, read as the tenant (WI#390) | `read_tenant_env_value "$name" "$env_path" KEY` (`06-env-generation.sh:335`) for `GATEWAY_ENABLED`, `GATEWAY_HOST`, `LUNARWING_BASE_DIR`, `GATEWAY_CLIENT_TOKENS_FILE`, and (master mode only) `GATEWAY_AUTH_TOKEN` |
| read and transform the tenant registry JSON as the tenant | `tenant_jq "$name" …` (`06-env-generation.sh:2435`) |
| write files without root following links | `write_tenant_file_atomic <account> <file> 600 {owned-parent\|private}` (`06-env-generation.sh:409`); input is a fully built here-string (the writer commits whatever it reads) |
| desktop directories as the user | `tenant_mkdir_p` (`06-env-generation.sh:465`), `tenant_chmod <account> <mode> <file>` (`:2456`) |
| symlink-free path check | `darkirc_path_components_safe` (`05-tenant-build.sh:1005`) |
| token generation | `generate_token` (`00-common.sh:272`), 64 lowercase hex |
| authenticated probe without argv exposure | the `_wait_tenant_gateway` pattern `curl -K <(printf 'header = "%s"\n' …)` (`04-tenant-lifecycle.sh:915`) |
| listener owner before any token is sent (SEC-01) | NEW `_tui_port_owner_uid` (awk over `/proc/net/tcp` and `/proc/net/tcp6`), compared with `id -u <tenant>` |

The helper names say "tenant" but take the account to drop to as their first argument, so they serve the desktop user too.

Functions (about 320 lines):
- `tui_access_command "$@"`.
- `_tui_desktop_user_resolve <user>`: name `^[a-z_][a-z0-9_-]{0,31}$`; `getent passwd`; refuse uid 0, a uid below `UID_MIN` from `/etc/login.defs` (default 1000), or a registered tenant name; home absolute, path-safe and owned by that uid; sets `TUI_USER_HOME`, `TUI_USER_UID`.
- `_tui_gateway_url <tenant> <env_path> <override_host>`: refuse `GATEWAY_ENABLED=false`; numeric port; `""`, `0.0.0.0`, `127.0.0.1`, `localhost`, `::`, `[::]` → `127.0.0.1`; `::1` → `[::1]`; other IPv6 → `[addr]`; IPv4/hostnames matching `^[A-Za-z0-9.-]{1,253}$` kept; `--url-host` overrides.
- `_tui_client_tokens_path <tenant> <env_path>` (SEC-10): requires `LUNARWING_BASE_DIR == $(tenant_state_dir)`; `GATEWAY_CLIENT_TOKENS_FILE` unset or equal to `$(tenant_state_dir)/gateway-client-tokens.json` → that path; `none` → refuse client mode and point to `--use-master-token`; any other value → refuse with "GATEWAY_CLIENT_TOKENS_FILE must be unset, none, or <state>/gateway-client-tokens.json" (the daemon refuses other locations too, 8.3.5).
- `_tui_registry_upsert` and `_tui_registry_remove`: the jq filter fails unless `.version==1` and `.tokens` is an array; replaces the entry with the same label; `length <= 32`; written with the `owned-parent` policy.
- `_tui_registry_list <tenant>` (SEC-13): reads the registry as the tenant with `tenant_jq` and prints, per entry, the label, `created_at` and `expires_at` only if the label passes `test("^[a-z0-9][a-z0-9:._-]{0,63}$")` and each present timestamp passes `test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")` (absent ones print `-`); anything else prints `<invalid entry #n>`. Raw tenant-written strings never reach root's terminal (jq decodes JSON escapes into control bytes).
- `_tui_desktop_config_write <user> <home> <tenant> <json>`: `$home/.config/wingtip/tenants`, `tenant_mkdir_p` + `tenant_chmod 700` on both directories, writer with the `private` policy; `_tui_desktop_config_remove`.
- `_tui_port_owner_uid <port>` (SEC-01): `awk` over `${TUI_PROC_NET_DIR:-/proc/net}/tcp` and `tcp6` (the override exists for the shell test) for LISTEN rows (`$4 == "0A"`) whose local port, the hex after the `:` of `$2`, equals the port; prints the single owning uid (`$8`), `none`, or `conflict` when several uids hold it.
- `_tui_gateway_probe <tenant> <url> <token>`: `GET /api/health` with a 3 s timeout (no token); then, only when the URL is loopback and `_tui_port_owner_uid <port>` equals `id -u <tenant>`, up to 4 tries 2 s apart of `GET /api/gateway/status` with the bearer passed through `curl -K` (200 → 0; 401/403 after all tries → 2). Otherwise it warns "gateway port <p> is held by uid <u>, not <tenant> (<uid>); skipping the authenticated probe" and returns 0 without sending the token. A non-loopback `--url-host` also skips the authenticated probe.

Secret handling: `token="$(generate_token)"`; `digest="$(printf '%s' "$token" | sha256sum | cut -d' ' -f1)"` (stdin, not argv; digest must match `^[0-9a-f]{64}$`); the desktop JSON is built with the `printf -v` builtin from validated values (tenant `[a-z0-9-]`, URL charset checked, token hex (master mode `^[A-Za-z0-9._~+/=-]{16,512}$`), label `tui:<user>`, `tenant_uid` from `id -u <tenant>` (digits only), `date -u +%Y-%m-%dT%H:%M:%SZ`) and passed as a here-string; the token never reaches argv, a log or stdout; `unset token json` after the probe.

Init agnosticism and registry awareness: the verb never starts, stops or probes a unit and never calls `systemctl` or `rc-service`; grants and revocations take effect through the daemon's 2 s file reload, so systemd user units and OpenRC behave identically. The only network action is an HTTP probe of the registry `gateway` port. `ports_registry_require_readonly` fails on a missing or invalid registry instead of migrating it. If a future revision needs a restart it must use `_restart_tenant_daemon` (`04-tenant-lifecycle.sh:946`), which branches on `INIT_SYSTEM`.

### 9.4 Revocation, rotation, expiry

- `--revoke`: remove the `tui:<user>` entry tenant-side (the daemon stops accepting the token within about 2 s and ends that credential's open SSE streams), then delete the desktop file. For a `token_kind:"master"` file, warn that the master token stays valid until `GATEWAY_AUTH_TOKEN` is rotated by hand plus `restart-tenant`.
- Rotate: re-run the grant; it replaces the digest for the same label atomically and rewrites the desktop file. A running TUI's open stream ends within about 2 s because the old digest is no longer active (8.3.5); the TUI reconnects, gets 401, re-reads the desktop file once and continues with the new token.
- `--expires-in-days n` writes `expires_at`; the daemon skips expired entries and ends their open streams at the expiry time. No default expiry (open question A.5; the docs recommend 90 days on laptops).
- Emergency: `tui-access <t> --for-user <u> --revoke` for one user; `mt-admin stop-tenant <t>` for everything (both init systems).

### 9.5 File formats

Tenant side, `/home/<t>/lunarwing/state/gateway-client-tokens.json`, 0600, owned by the tenant, no secrets:

```json
{"version":1,"tokens":[{"label":"tui:sun","sha256":"3f1c…64 hex…","created_at":"2026-11-03T00:15:00Z","expires_at":"2027-02-01T00:15:00Z"}]}
```

Desktop side, `~sun/.config/wingtip/tenants/<tenant>.json`, 0600 in a 0700 directory chain, owned by `sun`:

```json
{"schema":"wingtip/tenant-access","version":1,"tenant":"derpy","transport":"gateway",
 "url":"http://127.0.0.1:10000","tenant_uid":1010,"token":"<64 hex>","token_kind":"client","label":"tui:sun",
 "issued_at":"2026-11-03T00:15:00Z","expires_at":"2027-02-01T00:15:00Z"}
```

Client loading rules: section 3.11.3. `tenant_uid` is required; the client refuses a file without it (SEC-01).

### 9.6 Dev daemons for socket testing

Sockets are for same-uid daemons. A sun-owned dev daemon runs through the existing harness `ic/scripts/lunarwing-xmpp-test-env.sh` (isolated base dir, socket, DB and ports; default gateway 8765, outside the registry ranges). The harness defaults to the debug profile, which AGENTS.md forbids; use `LUNARWING_TEST_PROFILE=release` with a release binary built once in tmux, which needs Christopher's approval because AGENTS.md reserves release builds for tenant deploys (decision DEC-2):

```bash
cd ic
tmux new-session -d -s lwbuild "taskset -c 0-11 bash scripts/vendor-artifact-guard.sh --profile release; \
  taskset -c 0-11 cargo build --release -j12 --bin lunarwing 2>&1 | tee /tmp/lw-dev-build.log"
export LUNARWING_TEST_ROOT="$HOME/.local/state/wingtip-dev" LUNARWING_TEST_PROFILE=release \
       LUNARWING_TEST_DATABASE_KIND=libsql LUNARWING_TEST_ENABLE_PROXY=false \
       LUNARWING_TEST_TENSORZERO_URL=http://<tensorzero-host>:3000
scripts/lunarwing-xmpp-test-env.sh init
scripts/lunarwing-xmpp-test-env.sh start-lunarwing      # socket: $LUNARWING_TEST_ROOT/run/lunarwing.sock ; gateway: http://127.0.0.1:8765
scripts/lunarwing-xmpp-test-env.sh stop-lunarwing
```

A dev client-token file: `umask 077; tok=$(head -c32 /dev/urandom | od -An -tx1 | tr -d ' \n'); dig=$(printf %s "$tok" | sha256sum | cut -d' ' -f1); printf '{"version":1,"tokens":[{"label":"tui:dev","sha256":"%s"}]}\n' "$dig" > "$LUNARWING_TEST_ROOT/state/gateway-client-tokens.json"`, then a `tokens/devgw.token` file with `$tok` and the `devgw` profile of 3.11.

---

## 10. Security analysis

### 10.1 Assets and trust boundaries

| Asset | Exposure today | After this design |
|---|---|---|
| Tenant gateway (bearer only, 0.0.0.0 on LAN and NetBird) | master token is the only credential; anyone with it has full API access | per-user client tokens, header-only, route-scoped, revocable, rotatable and expiring in about 2 s including open streams; master token only by explicit flag |
| Loopback listeners (tenant gateways on high ports, Hex) | a client sends its token to whatever process holds the port, for example another tenant uid during a restart | listener owner uid checked against `/proc/net/tcp` before every token-bearing request, by the client and by `tui-access` (3.8.1, 9.3) |
| Unix socket | no chmod, no peer check; `start()` steals a live path (WI#385); unbounded reads; writer under locks | 0600, strict parent policy, same-uid gate, start lock, bind-first/probe, foreign-owner refusal, inode-checked unlink, bounded reads, writer task |
| Approvals | socket windows share one thread; bare keywords silently swallowed or applied to whatever is pending | one live connection per socket thread, request-id validated, typed errors; hidden-content gating, freshness rules and a confirmation for typed or dictated replies (3.6.2, 3.10.3) |
| Daemon availability | `/quit`, `/exit`, `/shutdown` over socket or gateway stop the daemon; so does a model reply that maps to `Ok(None)`; a stalled socket reader can freeze dispatch | commands refused for both channels; with DEC-1, `Ok(None)` from either channel continues (8.4); bounded queues, timeouts, lock release |
| Credentials in auth mode | a pasted secret reaches chat and the LLM if auth mode is lost first (Ctrl+S interrupt, Esc on the socket) | masked capture outside the store, every submit routed to the credential path, socket Esc interrupts, local time-outs below the daemon TTL; daemon debug log reduced in M4 (3.6.4) |
| Audio and transcripts | audio attachments persisted to the search-indexed workspace | never sent as attachments; in-memory only; LunarVoice on loopback |
| Images | declared MIME trusted, SVG accepted, ids collide, silent drops; persistence undisclosed | sniffed allowlist on client and both servers, UUID ids, explicit 413/422; workspace persistence disclosed (5.6, DEC-3) |
| Terminal | model output, tool previews and approval text rendered raw | sanitized (ANSI, C0/C1, bidi) before reaching state |

### 10.2 Gateway and tokens

- Bearer auth is unchanged for the master token. Client tokens are additional, narrower credentials: digest-only at rest (`sha256` in a 0600 tenant-owned file inside protected state), constant-time comparison, fail closed on any file problem, no query-string use, no WS, 403 outside chat, voice and status.
- Blast radius of a leaked client token: chat, voice, approvals and status as the tenant owner. Through the agent this still reaches tool execution, because approvals are reachable; it is not a sandbox. It cannot read or change settings, install extensions, read logs, use `/v1/*`, open project files or WS, and it can write an extension credential only while the agent is waiting for that credential (SEC-07); outside that window `auth-token` answers 403. Rotation, revocation, expiry and a failed reload end its open event streams within about 2 s (SEC-05). The token file is read only from a sandbox-protected directory (SEC-10).
- Listener ownership (SEC-01): tenant gateways listen on high ports of a shared host, so the client checks the owner uid of the listening socket before every token-bearing request (3.8.1), and `tui-access` checks it before its authenticated probe (9.3). Residual: the race between the check and the connect.
- CORS, origin checks, security headers, the 10 MiB body limit and the per-user rate limits (chat 30/min, voice 10/min, shared with the web UI) are unchanged. The approval-metadata fix turns some global broadcasts into user-scoped ones (narrower).
- Cleartext: `tui-access` writes `127.0.0.1` URLs unless the tenant binds a specific address; the client refuses plain `http` to non-loopback hosts unless `allow_insecure_http` is set. Rebinding tenant gateways to loopback is recommended as a separate change (decision DEC-5).
- Token at rest: 0600 file in a 0700 chain; the client refuses group- or world-readable files and never puts tokens in argv, URLs, logs, errors or child environments; `Secret` redacts in `toString`, `JSON.stringify` and `Bun.inspect`. Docs tell users to exclude `~/.config/wingtip/tenants/` from dotfile sync.
- Token in transit: Bun's `fetch` would route even loopback requests through an inherited `HTTP_PROXY`; the client neutralizes the proxy variables at startup (3.11.4, SEC-14).
- Correlation ids are charset-restricted, never interpreted and not secrets. `message_id` is a server UUID.

### 10.3 Socket

- Same-uid gate on every connection; 0600 socket; missing parent created 0700; an existing parent must be owned by the daemon uid and not group- or world-writable, and is never chmodded; a start lock serializes concurrent starts; a socket owned by another uid at the path is refused, never unlinked (SEC-11). `lunarwing repl` checks the socket owner before connecting; the standalone `ic/unix-socket-client` has no uid API without a new dependency, and its README says so.
- The uid gate cannot distinguish rootless-podman sandbox processes (keep-id maps them to the daemon uid). The socket directories (`$XDG_RUNTIME_DIR`, `/run/user/<uid>`, the configured socket's parent, `/tmp/.X11-unix`, the tmux socket directory, and on tenants `run/`, `06-env-generation.sh:1624`) are protected paths: never mountable and shadowed under any mounted ancestor. `/`, `/run` and `/var/run` are refused as mount roots. The mask walk also masks every socket it reaches, but stops at depth 5 or 10 000 entries and then only warns (8.5.7, DEC-4). The precise claim: no socket in a known socket directory is reachable from the sandbox; a socket deep inside a large ordinary tree may be.
- Conditions a restart cannot fix (another daemon serving, a foreign owner, an insecure parent, a replaced path) are logged and reported healthy, so the single-node watchdog cannot restart the daemon in a loop (F8).
- DoS bounds per connection: 16 KiB handshake in 10 s, 8 pending handshakes, 16 MiB + 64 KiB frames, 8 messages and 20 MiB of attachments in flight, 60 messages and 10 transcriptions per minute, 180 s idle timeout (v3), outbound 1024 frames / 32 MiB with status shedding and a 15 s write timeout. Worst-case memory is bounded (10 connections × (16 MiB line + parse + decode), roughly 450 MiB); a same-uid client can kill the daemon anyway.
- Logging: no frame payloads, ever; kind, sizes, ids, session id, peer uid and pid only; redacting `Debug`. The sandbox-visible log copy (`ic/src/bootstrap.rs:176-183`) therefore never receives payloads.
- Credentials typed after `AuthRequired` are forwarded untouched and never echoed in `Status`; the channel never logs them. The daemon's `SubmissionParser` logs every message's text at debug level (`ic/src/agent/submission.rs:17`) until M4 reduces that line to the length (8.6.1).

### 10.4 Approvals and control inputs

- Gateway approvals are owner-scoped (same authority as the web UI); REPLv3 approvals are scope-scoped with request-id validation; `always` is clamped by the agent when not offered.
- Typed and dictated keywords (3.10.3): with a prompt pending they are never sent as chat; approve and deny need a confirmation naming the tool, the 1 s and typed-after rules, and the full view when content is hidden; typed text never yields `always`. With nothing pending: client guard (second Enter), REPLv3 `Error{no_pending_approval}`, gateway `turn_finished{no_reply}`.
- Desktop notifications carry the tenant and the tool name only.
- `/quit` family: refused by the daemon for gateway and socket, and never sent by the client (send-path guard asserted in both transports). `/restart` refused locally. With DEC-1, a model reply that maps to `Ok(None)` (V1 `NO_REPLY`, V2 `Completed{response:None}`) cannot stop the daemon from these channels either; without DEC-1 it still can (8.4).

### 10.5 Images and rendering

- Magic-byte allowlist on client and servers; SVG (active content that would be persisted base64 into the search-indexed workspace, `attachment_store.rs:126-139`) and BMP rejected; base64 lengths checked before decoding; per-image, count and total caps; decode bombs limited client-side by a 40-megapixel cap.
- Generated images: data URLs only, allowlisted types, 16 MiB cap on both engines; server `path` never loaded by the renderer.
- OpenTUI 0.8.1's native library contains a decode-only FFmpeg; the client feeds it only sniffed allowlisted images.
- Persistence: every image sent is stored in the tenant workspace with no expiry and is readable from every channel (5.6). The TUI discloses this on first use per source and in the attachment bar; whether TUI turns should skip storage is DEC-3.
- `/image open` hands validated bytes to the desktop's default viewer through `xdg-open`, an external decoder the user chose to invoke.

### 10.6 Voice and local helpers

- No audio on disk, no audio attachments; buffers zero-filled; at most one failed clip stays in memory, for up to 10 min (5.4.1); the mic is open only while recording (the Hex-recorded sources, which keep Hex listening, are deferred).
- Hex: token read from a validated 0600 owner-only discovery file or the embedded child's ready line, held as a `Secret`, sent only to `127.0.0.1`; pid liveness, EPERM and the listener-owner check (3.8.1) prevent sending the token or audio to a port another uid holds; children get an allowlisted environment without `TRANSCRIBE_DUMP_DIR`; no automatic model downloads; argv only, never a shell.
- Clipboard read only on explicit Ctrl+V; cliphist risk disclosed; `/shot` bypasses the clipboard.
- Child processes (`parec`, `wl-paste`, `wl-copy`, `grim`, `slurp`, `notify-send`, `xdg-open`, `hex`) are spawned with argv, byte caps and timeouts and the allowlisted environment, and are killed on quit (`renderer.on("destroy")`); the embedded Hex child also exits when its stdin lease closes.

### 10.7 Supply chain

Exact OpenTUI/React pins with a frozen lockfile; no new Rust crates; Hex is never installed by this work (docs give checksum-verified manual steps; the RPM is unsigned).

---

## 11. Testing strategy

Rules: no cargo `build` (debug); `taskset -c 0-11` and `-j12` on every cargo command; tmux for long runs; Rust tests use real implementations or stubs (no mocks), `tempfile` for temporary paths, and `#[cfg(test)]` builders for short time limits (CI runs lib tests with `--test-threads=1`, `.gitlab/ci/rust.yml:145`, so time-based tests must pass serially). TypeScript tests never touch the real microphone, clipboard, Hex, tmux, tenant gateways or sidecars: commands and the clipboard are injected and peers are loopback mocks.

Panic policy in separate test files (F4): the blocking `panic-policy` job (`.gitlab/ci/validate.yml:48-58`) runs `ic/scripts/check_no_panics.py`, which flags `unwrap`, `expect` and `assert!` on every added line under `src/` and `crates/` unless the item carries `#[test]`, `#[tokio::test]` or `#[cfg(test)]` in that same file or sits inside `mod tests` (`check_no_panics.py:15-25`); an inner `#![cfg(test)]` does not count. Therefore every non-`#[test]` item (helper fn, struct, impl, const) in a separate `tests.rs` or `tests/*.rs` file carries `#[cfg(test)]`, as `ic/src/bridge/router/outcome/tests.rs:101-102` already does; otherwise keep tests inline in `#[cfg(test)] mod tests {}`. Every Rust milestone runs `python3 ic/scripts/check_no_panics.py --base "$(git merge-base HEAD origin/master)" --head HEAD`.

### 11.1 Rust tests

M2a (lifecycle, gateway contract, V2):

| File | Tests |
|---|---|
| `ic/crates/lunarwing_common/src/client_caps.rs` | `client_id_accepts_uuid_and_short_ids`, `client_id_rejects_dot_colon_space_and_65_chars`, `turn_outcome_serializes_snake_case`, `unknown_outcome_deserializes_to_unknown`, `wire_limits_omit_transport_specific_none_fields` |
| `ic/src/channels/lifecycle.rs` | `hint_overrides_replied_and_no_reply`, `hint_never_overrides_failed_timed_out_cancelled`, `hint_is_consumed_once`, `hints_are_bounded_and_expire` |
| `ic/src/channels/manager.rs` | `message_lifecycle_reaches_owning_channel`, `message_lifecycle_applies_outcome_hint`, `message_lifecycle_for_unknown_channel_is_noop` |
| `ic/src/channels/image_mime.rs` | `sniffs_png_jpeg_gif_webp_only`, `rejects_svg_bmp_tiff_and_short_input`, `decode_checks_base64_length_before_decoding` (a 20 MiB string of `!` yields `TooLarge`, proving the order), `decode_uses_sniffed_mime_not_declared`, `decode_rejects_more_than_max_count`, `decode_enforces_total_bytes`, `decode_ids_are_prefixed_unique_uuids`, `decode_sanitizes_and_truncates_filenames` |
| `ic/src/agent/commands.rs` | `quit_refused_for_gateway_and_unix_socket_repl`, `quit_permitted_for_local_repl_and_other_channels` |
| `ic/src/agent/dispatch.rs` | existing `cancel_lane_*` tests adapted to `.len()`, plus `cancel_lane_returns_removed_messages` |
| `ic/src/agent/agent_loop.rs` (unit) | `turn_outcome_maps_results`, `none_result_is_continue_for_client_channels` (DEC-1) |
| `ic/tests/e2e_turn_lifecycle.rs` (NEW, `#[cfg(feature = "libsql")]`, `mod support;`, `TestRigBuilder` + `TestChannel::captured_lifecycle()`) | `lifecycle_started_then_finished_replied_for_simple_turn` (`simple_text.json` trace), `lifecycle_finished_exactly_once_per_message` (two messages), `lifecycle_finished_no_reply_for_bare_keyword_without_pending` (zero LLM calls), `lifecycle_finished_paused_when_tool_needs_approval` (`with_auto_approve_tools(false)`), `lifecycle_finished_failed_for_error_reply` (`error_path.json`) |
| `ic/tests/e2e_quit_refusal.rs` (NEW, libsql) | `socket_quit_is_refused_and_agent_keeps_running` (`with_channel_name("unix_socket_repl")`: `/quit` gets a reply containing "cannot stop"; a later `hello` is answered), `gateway_quit_is_refused_and_agent_keeps_running` (`with_channel_name("gateway")`), and with DEC-1 `socket_silent_reply_does_not_stop_agent` and `gateway_silent_reply_does_not_stop_agent` (NEW trace `ic/tests/fixtures/llm_traces/tui/silent_reply.json` whose final text is `NO_REPLY`; the turn finishes `no_reply` and a later `hello` is answered) |
| `ic/src/bridge/router/outcome/tests.rs` | `stopped_turn_reports_interrupted_hint`, `failed_turn_reports_failed_hint`, `detached_turn_reports_detached_then_late_finish` (extends the `Rig` of `late_gateway_response_uses_real_channel_once_with_original_thread`; new helpers carry `#[cfg(test)]`) |
| `ic/src/bridge/router.rs` tests | `gate_pause_reports_paused_hint`, `deliver_thread_event_correlates_state_changes`, `image_action_reaches_channels_as_image_generated_without_preview` |
| `ic/src/bridge/generated_images.rs` | `stash_take_matches_thread_and_call`, `stash_evicts_by_count_bytes_and_ttl` |
| `ic/src/bridge/effect_adapter.rs` | `image_sentinel_is_stashed_for_channel_threads_only`, `image_sentinel_output_to_engine_unchanged` |
| `ic/src/tools/builtin/image_gen.rs` | `parse_image_sentinel_accepts_png_data_url`, `parse_image_sentinel_rejects_other_tools`, `parse_image_sentinel_rejects_svg_and_oversize` |
| `ic/src/channels/web/sse.rs` | `correlated_broadcast_merges_message_ids_into_sse_payload`, `uncorrelated_event_payload_has_no_message_id`, `subscribe_raw_correlated_yields_correlation`, `correlation_from_metadata_rejects_bad_ids`, `subscribe_until_ends_stream` |
| `ic/src/channels/web/server.rs` | `chat_send_stamps_message_id_and_client_turn_id_in_metadata` (`TestGatewayBuilder::new().msg_tx(tx).start()`), `chat_send_rejects_invalid_client_turn_id`, `approval_stamps_owner_metadata_and_original_client_turn_id`, `auth_token_resume_stamps_client_turn_id` (F1: the queued resume message carries `message_id` and the sent `client_turn_id`), `auth_cancel_rejects_invalid_client_turn_id`, `gateway_images_reject_svg_with_422`, `gateway_images_reject_oversize_with_413`, `gateway_images_use_sniffed_mime`, `gateway_image_ids_are_unique_across_requests`, `chat_capabilities_reports_engine_features_limits_voice` |
| `ic/src/channels/web/tests/correlation.rs` (NEW; registered in `tests/mod.rs`; helpers `#[cfg(test)]`) | `send_status_carries_correlation_from_metadata`, `respond_carries_correlation_from_incoming_metadata`, `turn_finished_follows_correlated_response`, `lifecycle_not_emitted_without_message_id` |
| `ic/src/channels/web/mod.rs` | `status_sse_event_matches_previous_inline_mapping` (one case per `StatusUpdate` variant) |
| `ic/src/channels/web/voice.rs` | `voice_status_reports_stt_tts_and_rate_limit`, `validate_stt_input_rejects_empty_oversize_bad_base64_bad_format`, `transcribe_b64_maps_sidecar_failures` (429, 503, 504, 400, 422, 401, connection error); `ic/tests/voice_gateway_integration.rs` status case extended |

M2b (client tokens):

| File | Tests |
|---|---|
| `ic/src/channels/web/client_tokens.rs` | `client_token_authenticates_as_owner`, `client_token_file_group_readable_is_ignored`, `client_token_symlink_is_ignored`, `client_token_file_outside_protected_dirs_is_refused`, `reload_applies_revocation`, `expired_entry_is_rejected`, `malformed_file_fails_closed`, `too_many_entries_fail_closed`, `store_from_env_none_disables`, `is_active_requires_same_label_and_digest`, `failed_reload_deactivates_every_credential`, `credential_revoked_fires_at_expiry` (`tokio::time::pause`) |
| `ic/src/channels/web/auth.rs` (pattern `:293-330`) | `client_token_rejected_as_query_param`, `client_token_forbidden_outside_chat_voice_status` (403 on `/api/settings`), `client_token_allowed_on_auth_token_route`, `master_token_unaffected_by_client_tokens` |
| `ic/src/channels/web/server.rs` | `chat_capabilities_reports_client_credential`, `revoked_client_token_ends_open_event_stream`, `rotated_client_token_ends_open_event_stream`, `expired_client_token_ends_open_event_stream`, `failed_reload_ends_client_token_streams`, `client_token_auth_token_without_pending_request_is_403`, `client_token_auth_token_answers_pending_v1_auth`, `master_token_auth_token_unchanged` |

M3 (socket hardening, protocol 1):

| File | Tests |
|---|---|
| `ic/crates/lunarwing_common/src/repl_wire.rs` | `v1_frames_round_trip_byte_identical` (`v1_frames.ndjson`), `v3_frames_round_trip_semantically` (`client_frames.ndjson`, `server_frames.ndjson`, compared as `serde_json::Value`), `unknown_fields_are_ignored`, `unknown_error_code_maps_to_unknown`, `debug_output_redacts_payloads`, `approval_request_truncation_fields_default_false` |
| `ic/src/channels/unix_socket_repl/tests.rs` (every helper `#[cfg(test)]`) | listener: `socket_file_mode_is_0600`, `missing_parent_dir_is_created_0700`, `existing_parent_dir_mode_is_untouched`, `group_writable_parent_is_refused`, `world_writable_sticky_parent_is_refused`, `foreign_uid_peer_is_rejected` (`with_expected_uid(geteuid + 1)`, closed without a welcome), `same_uid_peer_is_accepted`, `peer_allowed_rejects_mismatched_uid`, `start_refuses_to_steal_live_socket`, `start_replaces_stale_socket`, `start_refuses_regular_file_at_socket_path`, `start_refuses_symlink_at_socket_path`, `foreign_owned_socket_is_refused` (pure `classify_existing` with an injected owner uid), `concurrent_starts_never_unlink_a_live_socket` (two `bind_socket` calls racing on one stale path: exactly one listener, the other `AlreadyServing`), `already_serving_reports_healthy_not_serving`, `shutdown_does_not_unlink_foreign_socket`, `replaced_socket_inode_is_logged_and_reported_healthy`; handshake/reader: `v1_client_gets_legacy_welcome_response`, `vibetui4_connect_without_protocol_is_v1`, `handshake_times_out_without_connect`, `non_connect_first_frame_closes_without_logging_payload`, `pending_handshakes_are_capped`, `v1_server_busy_response_when_full`, `oversize_line_is_rejected_and_connection_closed`, `oversize_handshake_line_closes_connection`, `crlf_terminated_frames_are_accepted`, `v1_session_has_no_idle_timeout`; writer: `respond_to_stalled_reader_returns_immediately` (each call < 50 ms while the client never reads), `slow_reader_is_disconnected_and_other_sessions_unaffected` (client B's Ping→Pong < 1 s throughout), `graceful_fatal_error_is_flushed_before_close`; v1 compatibility: `v1_approval_prompt_is_a_response`, `v1_approval_prompt_marks_truncated_parameters`, `v1_keyword_without_pending_gets_notice_response`, `v1_silent_turn_gets_no_reply_response`, `v1_attachments_get_legacy_error_response`, `v1_quit_is_forwarded_to_the_agent` (the refusal itself is pinned by M2a's `socket_quit_is_refused_and_agent_keeps_running`) |
| `ic/src/channels/manager.rs` | `respond_does_not_hold_registry_lock_while_channel_blocks`, `send_status_does_not_hold_registry_lock_while_channel_blocks` (a `GatedChannel` awaiting a `Notify`; `manager.remove("other")` finishes within 1 s) |
| `ic/src/sandbox/mount_guard.rs` | `masked_files_include_unix_sockets` (a `std::os::unix::net::UnixListener` bound at `<tmp>/ws/a/b/agent.sock`), `masked_files_skip_sockets_inside_skipped_dirs`, `tenant_logs_source_with_unix_socket_is_rejected`, `root_mount_masks_xdg_runtime_dir` (an injected runtime dir inside a temp root is returned by `masked_dirs_for_mount`), `runtime_dir_is_refused_as_mount_root`, `repl_socket_parent_is_protected`, `root_run_and_var_run_mount_roots_are_refused`, `socket_beyond_depth_cap_is_reported_truncated` (socket at `a/b/c/d/e/s.sock`, default caps), `socket_after_entry_cap_is_reported_truncated` (`ScanCaps { max_depth: 5, max_entries: 8 }`, nine files, then a socket) |
| `ic/src/cli/repl.rs` | `v1_encodings_match_golden_fixtures`, `socket_owner_check_refuses_foreign_uid_and_symlink` (pure `socket_owner_ok(meta, euid)`) |
| `ic/unix-socket-client/src/lib.rs` | `v1_encodings_match_golden_fixtures` (run in CI by `rust-test-lib`, F11) |

M4 (REPLv3):

| File | Tests |
|---|---|
| `ic/src/channels/unix_socket_repl/tests.rs` | negotiation: `v3_welcome_echoes_only_accepted_features`, `v3_welcome_reports_engine_v1_and_limits`, `v3_server_busy_error_when_full`, `invalid_thread_token_is_fatal_bad_frame`, `second_live_connection_on_same_thread_is_refused`, `thread_token_reusable_after_connection_closes`, `v3_idle_timeout_closes_session`, `malformed_json_gets_bad_frame_and_connection_survives`; correlation: `message_metadata_carries_session_msg_id_and_scope`, `ack_precedes_turn_started`, `two_messages_are_answered_separately`, `server_assigns_msg_id_when_absent`, `duplicate_in_flight_msg_id_is_rejected`, `in_flight_cap_returns_busy`, `paused_turn_does_not_hold_in_flight_slot`, `turn_finished_follows_response`; status: `status_events_equal_gateway_sse_events`, `stream_chunk_dropped_without_stream_feature`, `image_generated_is_control_class_and_bounded`, `status_not_sent_without_status_feature`, `status_frames_are_shed_and_gap_reported`, `turn_finished_reports_dropped_status`; attachments: `attachments_require_feature`, `attachment_mime_is_sniffed_not_declared`, `svg_and_bmp_are_rejected`, `attachment_ids_are_repl_image_uuids`; approvals: `approval_request_frame_carries_redacted_parameters`, `approval_request_parameters_are_not_truncated` (a 64 KiB parameter value arrives whole, `parameters_bytes` set), `oversize_approval_parameters_set_truncated_flag` (`#[cfg(test)]` small frame cap), `approval_frame_maps_to_exec_approval_submission`, `approval_from_other_thread_is_unknown_request`, `bare_keyword_without_pending_returns_no_pending_approval`, `internal_turn_approval_is_not_answerable_blind` (an unroutable `ApprovalNeeded` is dropped; `yes` gets `no_pending_approval` and is not forwarded), `stale_approval_gets_error_before_turn_finished`, `interrupt_cancels_pending_approvals_with_approval_resolved`, `auth_required_next_message_is_forwarded_untouched`; quit: `v3_quit_gets_forbidden_command_and_is_not_forwarded`; transcribe (a stub `VoiceApi` pointed at a loopback `axum` stub sidecar): `transcribe_returns_result_with_language_and_model`, `transcribe_not_advertised_without_voice_api`, `transcribe_without_feature_is_rejected`, `transcribe_oversize_rejected_before_decode`, `transcribe_second_request_on_same_connection_is_busy`, `transcribe_does_not_block_message_intake`, `transcribe_empty_text_is_no_speech`, `transcribe_sidecar_503_maps_to_busy`, `transcribe_cancelled_on_disconnect`; notices: `internal_message_without_session_routes_by_scope`, `unroutable_status_is_dropped`, `background_reply_is_notice_for_v3`, `undelivered_reply_is_replayed_on_reconnect_with_same_thread`, `undelivered_stash_is_bounded`; debug: `client_frame_debug_redacts_content_and_base64`, `server_frame_debug_redacts_text` |
| `ic/src/agent/submission.rs` | `parse_debug_log_omits_content` (`#[traced_test]`; parsing `ghp_secretvalue` leaves no log line containing it) |
| `ic/tests/e2e_unix_socket_repl.rs` (NEW, `#[cfg(all(unix, feature = "libsql"))]`; `TestRigBuilder::new().with_channel(Box::new(UnixSocketReplChannel::new(tmp.join("lw.sock"), 4, "test-agent", "test-user")))`) | `replv3_message_reply_round_trip_correlates_msg_id`, `replv3_queued_messages_each_get_their_reply`, `replv3_approval_round_trip_through_agent` (new trace `ic/tests/fixtures/llm_traces/replv3/approval_round_trip.json`; the tool runs once), `replv3_second_window_cannot_approve_first_windows_tool`, `replv3_bare_ok_without_pending_gets_error`, `replv3_quit_gets_forbidden_command`, `legacy_v1_client_receives_approval_prompt_as_response`, `replv3_reconnect_with_same_thread_replays_pending_approval` |

### 11.2 Rust commands

```bash
cd ic
taskset -c 0-11 cargo check -j12
taskset -c 0-11 cargo check -j12 --no-default-features --features libsql
taskset -c 0-11 cargo check -j12 --no-default-features --features postgres
taskset -c 0-11 cargo check -j12 --all-features
taskset -c 0-11 cargo test -j12 -p lunarwing_common -- --test-threads=12
taskset -c 0-11 cargo test -j12 --lib -- channels:: agent::commands agent::dispatch agent::agent_loop agent::submission bridge:: tools::builtin::image_gen sandbox::mount_guard cli::repl --test-threads=12
taskset -c 0-11 cargo test -j12 --features libsql --test e2e_turn_lifecycle --test e2e_quit_refusal -- --test-threads=4
taskset -c 0-11 cargo test -j12 --test voice_gateway_integration -- --test-threads=12
taskset -c 0-11 cargo test -j12 --features libsql --test e2e_unix_socket_repl -- --test-threads=4      # M4
taskset -c 0-11 cargo test -j12 --locked --manifest-path unix-socket-client/Cargo.toml                   # M3
taskset -c 0-11 cargo clippy -j12 --all --benches --tests --examples -- -D warnings
taskset -c 0-11 cargo clippy -j12 --all --benches --tests --examples --all-features -- -D warnings
taskset -c 0-11 cargo fmt --all -- --check
bash scripts/pre-commit-safety.sh
(cd .. && python3 ic/scripts/check_no_panics.py --base "$(git merge-base HEAD origin/master)" --head HEAD)
# full suite (long): tmux new-session -d -s tuitest "taskset -c 0-11 cargo test -j12 -- --test-threads=12 2>&1 | tee /tmp/tui-test.log"
```

### 11.3 TypeScript tests (`tui/test/`, `bun test`)

#### 11.3.1 Harnesses and mocks

```
test/setup.ts                   preload (bunfig.toml): unhandled rejections fail the run
test/helpers/render.tsx         renderApp(): testRender + fakes + ManualClock; act handling
test/helpers/frames.ts          locate(frame, text) -> {x, y}; rows(frame)
test/helpers/tmp.ts             mkdtemp(os.tmpdir()) registry, removed in afterEach
test/helpers/fake-spawn.ts      scripted Bun.spawn stand-in recording argv, env and stdin (parec, wl-paste, wl-copy, grim, slurp,
                                notify-send, xdg-open, hex)
test/fixtures/proc-net/*.txt    /proc/net/tcp and tcp6 text in this host's format (tenant listeners, ::1, v4-mapped, foreign uid)
test/fixtures/proxy-child.ts    child script for the proxy-environment test (3.11.4)
test/mocks/mock-gateway.ts      Bun.serve on 127.0.0.1:0: /api/health, /api/chat/{events,send,approval,history,threads,
                                thread/new,pending-gates,capabilities,auth-token,auth-cancel}, /api/voice/{status,transcribe};
                                modes "legacy" (today's server: no correlation, no lifecycle, 404 capabilities) and "current"
                                (this contract: correlation, turn_started/turn_finished after response, capabilities);
                                scripted SSE per request; records method, path, query and headers
test/mocks/mock-replv3-daemon.ts node:net server on a socket in a temp dir (salvaged v4 MockDaemon, v4:tui/test/repl-client.test.ts:10-70);
                                modes replv3 | legacy | busy | silent | garbage | thread_in_use; close(), rebind(), replacePath();
                                thread-resume stash; replays the golden NDJSON fixtures (M4)
test/mocks/mock-hex.ts          (M1b) Bun.serve on 127.0.0.1:0 + 0600 local-api.json in a temp HEX_APPLICATION_SUPPORT_DIR;
                                /health, /models, /transcriptions; enforces the bearer token on every route
test/mocks/fake-transport.ts    in-memory Transport whose emit() runs inside act()
test/mocks/fake-recorder.ts     Recorder replaying scripted Float32 chunks
test/mocks/fake-sources.ts      scripted VoiceSource
test/mocks/fake-clipboard.ts    scripted ClipboardReader and writer
```

#### 11.3.2 Unit tests

| File | Cases |
|---|---|
| `test/state/turn-text.test.ts` | the 9 vibetui4 TurnBuffer cases re-targeted (accumulate; empty final keeps deltas; non-empty final is canonical; final without deltas; late deltas rejected; duplicate final changes nothing; deltas before creation rejected; sealed record never reopened; new turn starts empty) |
| `test/state/reducer.test.ts` | "queued turn receives its own final" (C1); "response then turn_finished seals once on both transports"; "turn_finished before response still shows the late response"; "two finals for one id are joined"; "paused keeps the turn open and turn.started resumes it"; "no_reply with a pending approval stays awaiting_approval"; "detached shows background_running and the late finish seals"; "interrupted keeps deltas"; "cancelled shows cancelled before it started"; "interrupt while awaiting approval seals the paused turn"; "gate_resolved thread_terminated seals"; "gate_resolved expired seals with a note"; "auth-cancel 2xx seals the owning turn"; "paused turns do not count against the in-flight cap"; "three queued turns seal FIFO without correlation"; "exact ids override FIFO"; "interrupt ack seals the control and marks the running turn interrupted (legacy)"; "stray second Interrupted. swallowed once within 5 s (legacy V1)"; "steer ack attaches to the steer record"; "final with nothing inflight becomes background"; "a second final for a sealed turn does not eat the next reply"; "approvals deduplicate across live, history and pending-gates"; "approval 404 marks expired"; "error fails only its turn"; "other-thread events only bump the counter"; "conn/lost marks legacy socket turns lost and leaves gateway turns open"; "resync seals open turns from terminal history turns and keeps young Failed turns open"; "send failure restores the draft into an empty composer"; "send failure with a new draft moves the text to history"; "auth_required enters credential mode and stores no text"; "legacy: 15 s without events marks unanswered"; "uncorrelated response on a correlated gateway becomes background"; "image tool without image event adds the note"; "eviction keeps 500 records and 20 image payloads"; "notice expiry by level, errors sticky"; "reducer never reads Date.now (spy)" |
| `test/state/layout.test.ts` | rows at 80x24; composer grows to 6 rows and the transcript shrinks; attachment bar collapses below 16 rows; approval compacts below 14 rows; transcript ≥ 3 rows at 40x10 |
| `test/state/approval-measure.test.ts` | hidden lines and bytes counted at width 80; nothing hidden for short parameters; full view needs no measurement |
| `test/transport/framing.test.ts` | UTF-8 split at every byte offset of "café" plus U+1F600 (a 4-byte code point), "日本語" and a ZWJ emoji (C4); several lines per chunk and one line across many chunks; CRLF stripped; oversize line throws and resets; 8 MiB line in 64 KiB chunks is linear (C6) |
| `test/transport/sse.test.ts` | dispatch on blank line; multi-line data joined with `\n`; comments call `onActivity` only; split across chunks and code points; oversize event throws; `id`/`retry` ignored |
| `test/transport/gateway-wire.test.ts` | one case per row of the 3.4 gateway table; unknown types ignored; `image_generated` with an SVG data URL rejected; `gate_required` Authentication becomes `turn.auth_required`; `turn_finished` outcome strings map one to one; every `gate_resolved` resolution string normalizes as in 3.4 |
| `test/transport/listener-owner.test.ts` | the cases of 3.8.1 (IPv4, IPv6, wildcard, v4-mapped, foreign uid, no listener) from fixture text |
| `test/transport/backoff.test.ts` | 500 ms doubling capped at 15 s; jitter within ±20 %; reset after 30 s ready |
| `test/transport/guards.test.ts` | `isForbiddenContent` catches `" /EXIT "`, `/Quit`, `/shutdown`; not `/quitting` or `quit` |
| `test/voice/wav.test.ts` | clamping and rounding (-1 → -32768, 1 → 32767); 44-byte header fields; encode/parse round trip |
| `test/voice/levels.test.ts` | dBFS of full-scale sine and silence; odd-byte carry; no-signal after 1.5 s below -70 dBFS; peak hold decays after 1 s |
| `test/voice/vad.test.ts` | speech from the first frame is still sent; a muted input (peak < -50 dBFS) is discarded as no signal; 3500 ms silence after speech emits `silence` only with auto-stop on; floor adapts to hum |
| `test/voice/ptt.test.ts`, `test/voice/queue.test.ts` | the cases of 7.5 |
| `test/voice/fallback.test.ts` | busy LunarVoice is retried after 1, 2, 4 s and then falls back to the next source with the same WAV; busy exhausted on the last source keeps the clip and Alt+R resends the same WAV; 429 keeps the clip and the retry waits for the window; a full local window skips LunarVoice without a request; timeout falls back; no_speech surfaces without fallback and disposes the clip; auth falls back and raises a sticky warning; model_not_ready falls back with a one-time hint; too_large skips smaller-limit sources; fallback success raises the warn naming source and reason; end of chain raises one notice listing every attempt; Esc aborts the request and zero-fills the clip; Esc zero-fills the retained clip; the retained clip expires after 10 min; the 180 s budget ends the chain |
| `test/voice/hex-locate.test.ts` (M1b) | valid file accepted; 0640 refused; symlink refused; dead pid is stale; EPERM refused; a port held by another uid is refused before any request; token format and apiVersion validated; `HEX_APPLICATION_SUPPORT_DIR` override; a binary older than 3.3.0 leaves hex-embedded unavailable |
| `test/images/sniff.test.ts` | PNG, JPEG, GIF87a, GIF89a, WebP accepted; BMP, SVG, TIFF, HEIC, empty rejected; declared MIME ignored |
| `test/images/limits.test.ts` | per-image, count and total limits; gateway body budget includes base64 expansion and worst-case JSON escaping |
| `test/images/attach.test.ts` | PNG from a temp dir; FIFO rejected without blocking; bounded read when stat lies; quotes, `~` and `file://` handled; missing file message |
| `test/images/paste-paths.test.ts` | kitty newline-joined paths; percent-decoded `file://`; backslash-escaped and single-quoted paths; prose is not a drop; non-image extensions are not a drop; more than `maxImages` paths is not a drop |
| `test/images/clipboard.test.ts` | host clipboard image; `limit-exceeded` message; host failure falls back to wl-paste (injected spawn); wl-paste output over the cap kills the child; `text/uri-list` yields files; no `spawnSync` anywhere under `src/` (source grep) |
| `test/images/screenshot.test.ts` | region uses slurp then `grim -s 1 -g`; full uses `slurp -o` and `grim -s 1`; an oversize PNG is retried once as JPEG q85; the JPEG result attaches as image/jpeg; still too large reports the size |
| `test/images/export.test.ts` | open writes a 0600 file in a 0700 directory and spawns xdg-open with argv only; save refuses an existing path (`O_EXCL`); an evicted image reports that it is no longer in memory; exports are removed on dispose |
| `test/images/data-url.test.ts` | four types accepted; `image/svg+xml` and non-base64 rejected; oversize rejected; magic-byte mismatch rejected |
| `test/config/config.test.ts` | precedence; `--token` exits 2; token file 0644 refused, 0600 accepted, content never echoed; `LUNARWING_GATEWAY_TOKEN` blanked in `process.env`; child env holds no token variable, no proxy variable and no `TRANSCRIBE_DUMP_DIR`; plain http to a non-loopback host refused; tenant file validated (schema, version, mode, owner, size, expiry, `tenant_uid` present) and re-read once on 401; group-writable `config.toml` refused; profile names outside `^[a-z0-9_-]{1,32}$` refused; `Secret` absent from `JSON.stringify`, `String()` and `Bun.inspect` |
| `test/config/tenant-select.test.ts` | one tenant file is used without flags; five files open the picker and remember the choice; zero files exit 2 with the tui-access command; the 401 hint for a client token mentions `--use-master-token` |
| `test/config/instance-lock.test.ts` | a second instance is secondary and never writes `lastThreadId`; a stale lock with a dead pid is replaced; a symlinked lock path is refused |
| `test/config/proxy-env.test.ts` | with `HTTP_PROXY` pointing at a recording proxy, the child's requests to the mock gateway on `127.0.0.1`, `localhost` and `[::1]` never reach the proxy |
| `test/sanitize.test.ts` | ANSI CSI/OSC removed; C0/C1 and bidi controls removed; newlines only in multiline mode |
| `test/commands/slash.test.ts` | `/quit` and `/exit` quit without touching the transport; `/shutdown` refused and never sent; `/restart` refused and `//restart` needs a second Enter; `//help` forwards `/help`; `/approve` is structured on gateway and `yes` on legacy; unknown `/cmd` forwarded verbatim; `/clear` and `/new` are recorded as controls; the keyword guard needs a second Enter when nothing is pending; Ctrl+S in credential mode never POSTs `/api/chat/send` and never sends `/interrupt` |
| `test/commands/approval-reply.test.ts` | "'Yes.' with a pending prompt asks for confirmation and sends nothing"; "a second Enter approves the focused prompt"; "'No!' denies after confirmation"; "'Okay.' approves after confirmation"; "'Yes, but use staging.' is sent as text"; "bare 'a' with a pending prompt approves nothing and sends nothing"; "a prompt arriving while typing /approve is not approved"; "a prompt focused for under 1 s ignores Alt+Y"; "no normalisation when nothing is pending" |

#### 11.3.3 Integration tests

- `test/integration/gateway-transport.test.ts` (T1.3, mock gateway): connect opens SSE before any send, resolves thread `last` from the state file and seeds history; `new` creates a thread; `assistant` uses `assistant_thread`; capabilities mapped onto `ServerInfo` and 404 selects legacy mode; HTTP status mapping for send, approval, auth-token, auth-cancel and transcribe (200, 202, 400, 401, 403, 404, 413, 422, 429, 503 busy versus "not configured"); 401 re-reads the access file once and retries; `stream_gap` raises `resync.required`; image send carries the sniffed `media_type` and respects the body budget; `client_turn_id` is sent on send, approval, auth-token and auth-cancel; a listener owned by another uid never receives an Authorization header (injected `ProcReader`); the token never appears in a URL, the debug log or any error text; redirects are refused.
- `test/integration/gateway-session.test.ts` (T1.7, mock gateway + reducer + controllers): current mode: events keyed by `client_turn_id` attach even when they precede the 202; `turn_finished{paused}` keeps the approval open and the approval POST carries the owning turn's `client_turn_id`, the continuation lands on that turn; `auth_required` then Enter calls `/api/chat/auth-token`, never `/api/chat/send`; a credential continuation seals the owning turn; Esc Esc calls `/api/chat/auth-cancel` and the 2xx seals the owning turn; a `response` beginning with `Error: ` renders a failed turn; a second 401 goes offline with the hint for the token kind; 403 shows the scope error; legacy mode: the legacy badge, two queued sends each get their own `response` (FIFO), `status` Done without `response` triggers a resync after the grace, a bare keyword is marked `unanswered` after 15 s; other-thread events only bump the counter; send 429 restores the draft.
- `test/integration/gateway-reconnect.test.ts` (mock gateway + supervisor): dropped SSE → reconnecting → ready, history resynced, nothing re-sent; watchdog: 45 s without bytes forces a reconnect (`ManualClock`); 503 "Too many connections" retries after 30 s; 401 on `/api/chat/events` goes offline without a retry loop; a foreign listener owner during a reconnect goes offline with the uid message.
- `test/integration/hex.test.ts` (M1b, mock Hex): hex-app posts `audio/wav` with `Content-Length`, `model`, `language` and the bearer token; the model comes from a `verified` entry of `GET /models` and no `prepare` call is made; `queue-full` falls back; stale discovery makes hex-app unavailable; a 401 invalidates the discovery cache; embedded child (a fake `hex` script): ready line parsed, non-loopback URL rejected, foreign listener owner rejected, stdin lease closed on dispose, child exits.
- `test/integration/socket.test.ts` (M4, mock REPLv3 daemon; WI#458 item 5 cases marked): REPLv3 Welcome applies features and limits; legacy v1 welcome detected (images, approvals and voice disabled, warning shown); "Server busy" classified busy, never ready (C11); handshake timeout destroys the socket (C11); `msg_id` correlation with interleaved `Status{event}` frames for two turns decoded by the gateway mapper; `TurnFinished{no_reply}` without `Response` closes the turn; `TurnFinished{paused}` keeps it open; `Approval{request_id}` → `Ack`, `ApprovalResolved`, `TurnStarted` and `Response` for the original `msg_id`; `Error{no_pending_approval}` shown on that turn; `AuthRequired` puts the composer in credential mode, the next `Message` text is not stored, Esc sends `Interrupt`, and credential mode ends after 290 s without sending the text; a second instance of a profile gets its own thread token; `Error{thread_in_use}` backs off and goes offline after 3; `--thread last` sends `lastReplThread`, and after a reconnect the replayed `ApprovalRequest` and stashed `Response`/`TurnFinished` complete the open turn; `Notice{shutdown}` then close leads to reconnecting; `Error{server_busy, fatal}` treated as busy; features not accepted are never used; an `Error` frame fails its turn and is never rendered as an answer (D11); UTF-8 split across socket writes arrives intact; a frame above `max_frame_bytes` triggers a reconnect; a socket owned by another uid is refused; `EACCES` maps to `permission_denied` with the tenant hint; `ApprovalRequest{parameters_truncated:true}` can only be denied; **WI#458-5** daemon restart (close and rebind the same path → reconnecting, then ready with a new session id); **WI#458-5** path replaced while connected (WI#385 steal) → the next connect reaches the new listener; **WI#458-5** missing socket keeps retrying with backoff capped at 15 s and reset after 30 s ready; **WI#458-5** an in-flight legacy turn becomes `lost` and is never re-sent; **WI#458-5** REPLv3 thread resume keeps an in-flight turn open and delivers its `Response` on the new connection; **WI#458-5** two missing `Pong`s force a reconnect; `Interrupt` and `Steer` get their own `Ack`/`Response`/`TurnFinished` and never close the user's turn; `Transcribe{req_id}` resolves on `TranscribeResult` and `Error{req_id, busy}` maps to `VoiceError{busy}`; `/quit`, `/exit`, `/shutdown` are never written, including via `//quit`.
- `test/transport/replv3-wire.test.ts`, `test/transport/replv3-golden.test.ts` (M4): every client frame encodes to the 4.3.4 examples byte for byte; every server fixture in `../ic/crates/lunarwing_common/tests/fixtures/replv3/` decodes; `Status.event` goes through the gateway mapper; unknown variants and fields are ignored; ids outside `^[A-Za-z0-9_-]{1,64}$` are never generated.

#### 11.3.4 UI tests (OpenTUI `testRender`)

| File | Cases |
|---|---|
| `test/ui/app-smoke.test.tsx` (ported) | welcome, send, streamed reply survives an empty final |
| `test/ui/layout-resize.test.tsx` (**WI#458 item 6**) | at 80x24 the header is row 0, the footer is the last row, the composer border is visible and the newest turn sits right above it; after `setup.resize(50, 12)` and `setup.resize(120, 40)` plus `flush()` the same invariants hold; a three-line live reply is fully visible (C5); manual scroll-up is preserved while content arrives and PageDown to the end restores follow; at 50x12 pending attachments collapse to one line and the composer stays visible |
| `test/ui/composer.test.tsx` | a 120 s transcript wraps and stays fully reviewable; a multi-line paste keeps its line breaks; Enter sends and `\n` (Ctrl+J) inserts a newline; `\x1b\r` (Alt+Enter) inserts a newline; `\x1f` undoes inside tmux; a Ctrl+C clear is undoable and recallable with Up; the byte counter appears above 80 % |
| `test/ui/image-output.test.tsx` (modified) | generated image renders live and survives the final; a later final with no open turn is shown as a background message (inverts `v4:tui/test/image-output.test.tsx:196`); an image tool without an image event shows the note |
| `test/ui/input-keys.test.tsx` (kept + new) | the four vibetui4 pins; F9 and NUL never reach the composer; Ctrl+S sends the interrupt, then the message, in order; Ctrl+S on an empty composer sends only /interrupt; Alt+Y answers only while a prompt is pending, otherwise Alt+A is visual-line-home |
| `test/ui/approval.test.tsx` | tool, description and redacted parameters with bidi controls stripped; "always" hidden when `allowAlways` is false; mouse click on approve; hidden parameters disable Alt+Y until Alt+V; long parameters show +N lines and Alt+V shows them all; zero-width and tag characters render escaped; a truncated REPLv3 prompt can only be denied; a blurred terminal plus an approval spawns the notifier without parameters |
| `test/ui/credential.test.tsx` | a pasted credential never appears in a frame; Esc arms and a second Esc cancels; the buffer is cleared afterwards |
| `test/ui/info-panel.test.tsx` | F1 opens help and Esc closes it before cancelling dictation; `/voice status` lists every source with its reason; `/thread list` renders in the panel; PageDown scrolls the panel |
| `test/ui/footer.test.tsx` | one case per row of the 3.12.3 table |
| `test/ui/copy.test.tsx` | drag-select in the transcript writes to the injected clipboard writer; chrome is not selectable; `/copy` copies the last reply |
| `test/ui/no-color.test.tsx` | NO_COLOR frames carry no colour and every state keeps its label |
| `test/ui/ptt-ui.test.tsx`, `test/ui/voice-status.test.tsx` | the cases in 7.5 |
| `test/ui/degrade.test.tsx` | legacy: image gestures refused, no approval panel, stall notice after 120 s; no streaming shows activity and elapsed time; no transcribe and no Hex: the mic widget shows `(no mic)` and the voice panel lists the reasons |

#### 11.3.5 Hygiene (C12)

`testRender` sets `IS_REACT_ACT_ENVIRONMENT = true` (`otr081:test-utils.js:10`); component tests drive state inside `act()`; I/O-driven integration tests use `renderApp()`, which sets it to `false` after mounting (removes the 69 act() warnings). Temp directories come from `mkdtemp(os.tmpdir())` and are removed in `afterEach`; every `Bun.write` is awaited; no fixed `/tmp` paths; every test asserts observable output; timeouts use `ManualClock`.

#### 11.3.6 Commands and CI

```bash
cd tui
bun install --frozen-lockfile
./node_modules/.bin/tsc --noEmit
bun test
bun test test/integration/gateway-transport.test.ts      # one file
```

CI: NEW job `tui-ts` in `.gitlab/ci/validate.yml`, modelled on `worker-opencode-ts` (`.gitlab/ci/validate.yml:64-89`): `stage: validate`, `needs: []`, image `$CI_BUN_IMAGE` with `entrypoint: [""]`, tag `$CI_LINT_RUNNER_TAG`, the same three rules, no `before_script`, and the script as four separate lines `cd tui`, `bun install --frozen-lockfile`, `./node_modules/.bin/tsc --noEmit`, `bun test`. The validator pins every TS lane line by line (`.gitlab/scripts/validate_config.py:1304-1361`, `:1478-1560`; F10), so `WORKER_TS_LANE_SCRIPTS` gains `"tui-ts": ("$CI_BUN_IMAGE", ("cd tui", "bun install --frozen-lockfile", "./node_modules/.bin/tsc --noEmit", "bun test"))` in the same commit, and `.gitlab/README.md` (lane list near `:460`) describes the lane. Check with `python3 .gitlab/scripts/validate_config.py && python3 -m unittest discover -s .gitlab/tests -p 'test_*.py'` (the lane tests synthesize jobs from the tuple, `.gitlab/tests/test_goal1_ci.py:2131-2144`).

### 11.4 Shell tests (`ic/scripts/tests/test-mt-admin-tui-access.sh`, NEW, M1a)

Patterns: source `lunarwing-mt-admin.sh` and stub after sourcing (`test-mt-admin-daemon-pid.sh`); the WI#390 `sudo` stub runs drops as the invoking user (`test-mt-admin-gateway-wait.sh`); a `curl` stub records argv and copies the `-K` config; `PASS:`/`FAIL:` lines and a `RESULT:` summary; failure paths run in a subshell because `die` exits. CI picks the file up through `.gitlab/scripts/shell-harnesses.sh:7`, and `ic/scripts/check-mt-admin.sh` shellchecks the new module (it globs `mt-admin-lib/*.sh`).

Stubs: `require_root`, `ports_registry_require_readonly`, `with_tenant_lifecycle_lock() { shift; "$@"; }`, `tenant_exists_in_registry` (true only for `alpha`), `ports_get` (gateway → `20042`), `tenant_env_dir`/`tenant_state_dir`/`tenant_lw_root` into the fixture, `getent` (fake `desk` user, uid 1000), `id` (`id -u alpha` → 1500), `stat` shadowed for the home-owner check, `sleep` no-op, scripted `curl` codes. `_tui_port_owner_uid` reads `${TUI_PROC_NET_DIR:-/proc/net}/tcp` and `tcp6`, so the test points it at fixture files with port `20042` (hex `4E5A`) owned by uid 1500, or by another uid.

Assertions:
1. `grant writes 0600 desktop config with http://127.0.0.1:20042 and a 64-hex token`
2. `desktop config directories are 0700`
3. `tenant registry holds sha256(token) under label tui:desk and no plaintext token`
4. `token absent from stdout and stderr of the grant`
5. `token absent from every recorded argv (curl, jq, sha256sum stubs)`
6. `verify probe sends Authorization: Bearer via curl -K config`
7. `GATEWAY_HOST=0.0.0.0 -> 127.0.0.1; 100.88.107.159 kept; ::1 -> [::1]; --url-host overrides`
8. `re-grant rotates: new token, exactly one tui:desk entry, previous digest gone`
9. `revoke removes registry entry and desktop file; second revoke is a no-op`
10. `refuses --for-user root, a tenant name, an unknown user, uid below UID_MIN`
11. `refuses a symlinked ~/.config component`
12. `refuses when the destination file is a symlink (writer exit 73)`
13. `--use-master-token writes token_kind master with GATEWAY_AUTH_TOKEN, leaves registry untouched, prints warning`
14. `401 on every verify attempt -> exit 2 with upgrade/--use-master-token guidance; files kept`
15. `GATEWAY_ENABLED=false -> refuses`
16. `LUNARWING_BASE_DIR != tenant_state_dir -> refuses`
17. `GATEWAY_CLIENT_TOKENS_FILE=none -> refuses client mode, suggests --use-master-token`
18. `--list prints validated labels and dates, never digests or tokens`
19. `registry file with version 2 or non-array tokens -> refuses without rewriting it`
20. `probe skipped when the port owner uid differs: no Authorization header recorded, warning printed, exit 0, files kept`
21. `--list with an ESC byte in a label prints <invalid entry #1>`
22. `explicit GATEWAY_CLIENT_TOKENS_FILE outside state/ is refused`
23. `desktop config records tenant_uid 1500 from id -u alpha`

Commands: `cd ic && bash scripts/tests/test-mt-admin-tui-access.sh && bash scripts/check-mt-admin.sh`.

---

## 12. Docs to update (same branch as the change)

| Doc | Change | Milestone |
|---|---|---|
| `tui/README.md` (replaced) | install (`bun install --frozen-lockfile`), launch, tenant selection and `tui-access`, config files and validation, keys and the composer, commands, the info panel, capability matrix, PTT environments, privacy table (5.6), image limits and storage, vision caveat (6.5), copying text (kitty Shift+drag), desktop notifications, `NO_COLOR`, troubleshooting | M1a; Hex discovery, embedded lifecycle and the tested Hex version in M1b; transport section in M4 |
| `.gitlab/README.md` | the `tui-ts` lane next to `worker-opencode2-ts` (`:460`) | M1a |
| `docs/guides/MT-ADMIN-QUICKSTART.md` | `tui-access` grant/revoke/list next to `tokens` (around `:202-208`); `tokens` stays the web-login path | M1a |
| `docs/ops/MULTITENANCY-PRODUCTION.md` | Step 6 "Access gateways" (`:182-194`): TUI access, revocation, rotation, master-token fallback, the 0.0.0.0 note | M1a |
| `docs/ops/TENANT-CONFIGURATION.md` (F14) | "Gateway (Web UI)" (`:262-288`): the `tui-access` grant and revoke flow next to the `GATEWAY_AUTH_TOKEN` lookup (M1a); `GATEWAY_CLIENT_TOKENS_FILE` default, the `none` value, the allowed location and the file format (M2b) | M1a, M2b |
| `docs/guides/GATEWAY_CHAT_API.md` (NEW) | normative chat contract: 4.1 and 4.2 (correlation including credential continuations, lifecycle, capabilities, image rules, never-send list) in M2a; client-token scope and the `auth-token` rule in M2b | M2a, M2b |
| `docs/guides/GATEWAY_VOICE_API.md` | 401 and route-limit 413/422 are plain text; new status fields; the rate-limit slot is consumed before the configuration check (M2a); auth paragraph corrected, client tokens described (M2b); "Socket transcription (REPLv3)" (LunarVoice only, bounds, error codes) (M4) | M2a, M2b, M4 |
| `ic/src/NETWORK_SECURITY.md` | trust table (`:15-22`): MT gateways can be LAN-bound; rate limits are per user (`:124`) (M2a); gateway auth section (`:41-62`): client tokens, scope, revocation, rotation, expiry, the listener-owner check (M2b); "Unix-socket REPL": 0600 and the strict parent policy, the start lock, `SO_PEERCRED` and its rootless-container limit, bind-first and probe, limits table, `mount_guard` socket directories and the truncation limit, the not-serving health behaviour (M3) | M2a, M2b, M3 |
| `docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md` | the `message_lifecycle` contract: when `Started`/`Finished` fire, outcome meanings, hints, the detached exception, ordering after replies; the DEC-1 guard | M2a |
| `ic/unix-socket-client/README.md` | speaks protocol 1; links the REPLv3 spec; v1 compatibility behaviours (approval prompt as `Response`, no-reply notices, attachments refused); does not verify the socket owner | M3 |
| `ic/src/sandbox/mount_guard.rs` module docs, `ic/src/channels/unix_socket_repl/mod.rs` module docs | "credential files and Unix sockets", the protected socket directories, the scan limits; channel architecture | M3 |
| `docs/specs/REPLV3_PROTOCOL.md` (NEW) | normative REPLv3 spec (4.1, 4.3) with the fixtures as an appendix | M4 |
| `docs/guides/MT-ADMIN-QUICKSTART.md` (socket note) | attaching to a tenant socket requires the tenant uid (`sudo -u <t> … lunarwing repl --socket /home/<t>/lunarwing/run/lunarwing.sock`); other uids are refused | M4 |
| `ic/src/setup/README.md` | unchanged (onboarding does not change); stated in each MR | all |

---

## 13. Milestones

Order rationale: M1a delivers a usable TUI against today's tenant daemons with no Rust change (gateway in legacy mode, LunarVoice, images, `tui-access` in master mode), because the client negotiates capabilities and degrades; its acceptance is the checkpoint before any Hex work (UX-09). M1b adds Hex as the local voice backend; M1a and M1b ship together as the first client release, so both voice backends are there from day one (brief decision 2). M2a makes the gateway exact; M2b adds client tokens so tenants no longer need the master token. M3 hardens the socket on protocol 1, and M4 adds REPLv3 and the client socket transport. Every daemon milestone is independently revertible because clients detect legacy daemons (capabilities 404, v1 welcome).

### M1a: client gateway slice, LunarVoice, images, `tui-access`

Scope: section 3 (except 3.9), section 5 without the Hex sources, section 6 (client side), section 7, section 9 (both modes; client-token mode becomes effective with M2b), the `tui-ts` CI lane, `tui/README.md`, the multi-tenant docs.

Acceptance criteria:
1. In `tui/`: `bun install --frozen-lockfile`, `tsc --noEmit` and `bun test` pass on Bun 1.3.14 (host) and in CI (`oven/bun:1.4.4`); the `tui-ts` lane is pinned, and `validate_config.py` and the `.gitlab/tests` suite pass.
2. All tests of 11.3 except the M1b Hex files and the M4 socket files pass, including the C1-C12 pins, WI#458 item 6 (`layout-resize.test.tsx`), the composer, info-panel, footer, credential, approval, copy and voice-status tests, the listener-owner and proxy-environment tests, and the gateway reconnect cases, against the mock gateway in both "legacy" and "current" modes.
3. No `spawnSync` import under `tui/src/`; no token in any request URL, log line or error text; no token reaches a listener owned by another uid or a proxy (asserted by tests).
4. `ic/scripts/tests/test-mt-admin-tui-access.sh` passes all 23 assertions; `ic/scripts/check-mt-admin.sh` is clean.
5. Supervised checkpoint (16.1, 16.2, 16.3, 16.4, 16.5, 16.6, 16.11, 16.12, 16.14): `tui-access <t> --for-user sun --use-master-token` on one tenant; the TUI connects (legacy badge), sends a text turn, answers an approval, dictates through LunarVoice (the busy and offline paths show their notices and a failed clip retries with Alt+R), attaches an image with `/img` and `/shot`, renders an image and copies reply text. Test 16.5 records whether the default model sees images; if it does not, A.19 is decided before M1a closes.

Verification:

```bash
cd tui && bun install --frozen-lockfile && ./node_modules/.bin/tsc --noEmit && bun test
! grep -rn "spawnSync" src/    # must print nothing
cd .. && python3 .gitlab/scripts/validate_config.py && python3 -m unittest discover -s .gitlab/tests -p 'test_*.py'
cd ic && bash scripts/tests/test-mt-admin-tui-access.sh && bash scripts/check-mt-admin.sh
```

### M1b: Hex voice sources

Scope: 5.3.2 and 5.3.3 (`hex-app`, `hex-embedded`) and the Hex part of `tui/README.md`.

Acceptance criteria:
1. `test/voice/hex-locate.test.ts` and `test/integration/hex.test.ts` pass, the rest of `bun test` still passes, and `tsc` is clean.
2. With Hex absent (this host), the voice panel lists `hex-app` and `hex-embedded` as unavailable with their reasons, and dictation through LunarVoice is unchanged.
3. Opt-in supervised 16.9 against the released `hex-3.3.0-linux-x64` asset if Christopher installs Hex: the embedded ready line is accepted, `GET /models` lists a verified model, one `POST /transcriptions` succeeds on CPU, and the tested version is recorded in `tui/README.md`.

Verification: `cd tui && ./node_modules/.bin/tsc --noEmit && bun test`.

### M2a: gateway contract in the daemon

Scope: 4.1, 4.2 without client tokens, 8.2, 8.3.1-8.3.4, 8.3.6, 8.3.7, 8.4 (with the DEC-1 guard if approved); the docs of 12 marked M2a.

Acceptance criteria:
1. All M2a tests of 11.1 pass; `cargo check` for default, libsql-only, postgres-only and all features; both clippy runs with `-D warnings`; `cargo fmt --check`; `scripts/pre-commit-safety.sh` and `check_no_panics.py` clean.
2. For every gateway message carrying G1 metadata, on V1 and V2: `turn_started`, then progress, then any `response`, then exactly one `turn_finished` (two for `detached`), all with `message_id` and the sent `client_turn_id`; a V2 credential continuation carries the owning turn's id.
3. `GET /api/chat/capabilities` returns the 4.2.2 document with `credential:"master"`.
4. Invalid images fail with 413/422 naming the index; ids are `web-image-<uuid>`; V2 delivers `image_generated` without a `tool_result` preview.
5. `/quit`, `/exit`, `/shutdown` from `gateway` and `unix_socket_repl` are refused and the agent keeps running (e2e); with DEC-1 a `NO_REPLY` reply on those channels does not stop it either; the in-process REPL still exits.
6. The M1a client passes its "current" mode tests unchanged.
7. Supervised (16.6, 16.10, 16.11): upgrade one tenant through `mt-admin upgrade-tenant`; the TUI runs in exact mode with the master-token file; V2 generated images render; the web UI smoke test (send, image, approval) is unaffected.

Verification: the commands of 11.2 without the M3/M4 lines, plus `cd ../tui && bun test`.

### M2b: client tokens

Scope: 8.3.5 and the client-token parts of 4.2; the docs of 12 marked M2b.

Acceptance criteria:
1. All M2b tests of 11.1 pass; checks, clippy, fmt, safety and panic-policy as in M2a.
2. Client tokens authenticate on chat, voice and status routes, get 403 elsewhere and cannot use `?token=`; a revoked, rotated or expired token's SSE stream ends within about 2 s; a failed reload fails closed and ends client-token streams; `auth-token` with a client token answers 403 unless an authentication request for that extension is pending; a token file outside the protected directories is refused.
3. Supervised (16.7): re-grant without `--use-master-token` on the upgraded tenant; `curl` with the client token to `/api/settings` gets 403; `--revoke` and a rotation while the TUI is connected end its stream; the web UI is unaffected.

Verification: the commands of 11.2 without the M3/M4 lines.

### M3: socket hardening on protocol 1

Scope: 8.5 (all of it), `repl_wire` with v1 and v3 types and fixtures, the standalone client's CI line, the docs of 12 marked M3.

Acceptance criteria:
1. All M3 tests of 11.1 pass; checks, clippy, fmt, safety and panic-policy as in M2a; `cargo test --locked --manifest-path unix-socket-client/Cargo.toml` passes locally and in `rust-test-lib`.
2. Protocol 1 bytes are unchanged: `v1_frames_round_trip_byte_identical` and both legacy-client fixture tests pass.
3. WI#458 items 1-4: socket mode 0600; strict parent policy; foreign uid rejected; concurrent starts never unlink a live socket; a second start refuses to steal a live socket, replaces only stale socket inodes and leaves the channel not serving but healthy; shutdown never unlinks a foreign socket; the protected socket directories are refused as mount roots and shadowed under mounted ancestors; `/`, `/run` and `/var/run` are refused; sockets the walk reaches are masked and truncation is logged.
4. D1, D2, D8, D9: a stalled reader never delays another session or `respond()`; oversize lines close the connection; handshakes time out; no payload in logs.
5. `lunarwing repl` receives approval prompts (with the truncation marker when long) and no-reply notices instead of hanging, and refuses a socket owned by another uid.
6. Supervised (16.8, needs DEC-2; 16.13): two `lunarwing repl` sessions against a dev daemon; an approval prompt round trip; a second daemon instance on the same socket path reports "already serving" and stays healthy; a sandboxed command cannot reach the sockets in `$XDG_RUNTIME_DIR`.

Verification: the commands of 11.2 including the `--manifest-path unix-socket-client/Cargo.toml` line.

### M4: REPLv3 and the client socket transport

Scope: 4.3 server behaviour, 8.6, `main.rs` voice wiring, the parser debug line, client 3.9, the docs of 12 marked M4.

Acceptance criteria:
1. All M4 Rust tests and `e2e_unix_socket_repl` pass; checks, clippy, fmt, safety and panic-policy.
2. `tui/test/integration/socket.test.ts` (including every WI#458-5 case) and the golden tests pass against `ic/crates/lunarwing_common/tests/fixtures/replv3/`.
3. Against a pre-M4 daemon (HEAD or M3, both answer a v3 `Connect` with the v1 welcome `Response`), the TUI detects the legacy daemon and degrades to text only.
4. Two v3 windows cannot approve each other's tools or share a thread token; bare keywords get `no_pending_approval`; approval parameters arrive whole; `Transcribe` works through LunarVoice when configured and is not advertised otherwise; the parser no longer logs message text.
5. Supervised (16.8): the TUI over the socket to a dev daemon: turn, image, approval, interrupt, credential, transcription, and a daemon restart with thread resume.

Verification: the commands of 11.2 with the M4 lines, then `cd ../tui && bun test`.

### Deferred (not scheduled)

- Socket Engine V2 opt-in (8.7), TTS playback, LunarVision enrichment for gateway and socket images, broadcast notices for v1 sessions, a `GATEWAY_AUTH_TOKEN` rotation verb, a server-side `gate_resolved` for gates discarded by a V2 interrupt (the client seals those turns itself, 3.5 rule 3b).
- Hex-recorded voice sources (UX-09). When they are scheduled they need:
  - `hex-dictations` (a `CaptureVoiceSource` with a `CaptureSession`): `POST /dictations` with `Authorization: Bearer <api token>`; every `/dictations/{id}/*` call (`levels`, `heartbeat` every 3 s against the 10 s lease, `finish`, `cancel`) sends both `Authorization: Bearer <api token>` and `X-Hex-Dictation-Token: <ownerToken>` (`hex:src/local_api.rs:577-582`, `:612-615`; `hex:src/local_api/http.rs:266`; F12); the mock enforces both, with the test "missing owner token yields 404".
  - `hex-cli`: a crash lease (SEC-12), because Hex has no maximum dictation duration (`hex:src/linux/input.rs:639-647`): write `{source:"hex-cli", startedAt}` to the profile state file before `hex dictation start` and clear it after `stop` or `cancel`; at every TUI start, a leftover marker runs `hex dictation cancel --json` (5 s) and is cleared; `process.on("exit")` and `SIGTERM`/`SIGHUP` handlers spawn the cancel; prefer `hex-dictations` whenever `/capabilities` reports `serviceCapture`; test "a stale hex-cli marker triggers cancel at startup".
  - Their privacy rows (Hex History kept for one week, `live.ndjson`) return to 5.6.

### Size estimates

| Milestone | Production | Tests |
|---|---|---|
| M1a | about 6 400 TypeScript lines, 330 shell lines | about 5 600 TypeScript lines, 340 shell lines |
| M1b | about 700 TypeScript lines | about 600 TypeScript lines |
| M2a | about 1 450 Rust lines | about 1 500 Rust lines |
| M2b | about 550 Rust lines | about 650 Rust lines |
| M3 | about 1 450 Rust lines (minus 711 deleted) | about 1 250 Rust lines, 80 fixture lines |
| M4 | about 1 450 Rust lines, 950 TypeScript lines | about 1 600 Rust lines, 1 000 TypeScript lines |

M2a and M2b together are about 2 000 production lines, in line with the feasibility review's estimate for the former M2 (F15).

---

## 14. Task breakdown for parallel implementation agents

Rules: within a milestone no two tasks own the same file; a task may read any file. "Deps" lists tasks whose interfaces must exist first. The interfaces are fixed in this document, so dependent tasks can start against the spec and integrate when the dependency lands. Every task adds the tests listed for its files in section 11 and runs its milestone's verification for its scope. Deletions are owned like edits.

### M1a

| Task | Owns (files) | Deps | Deliverable |
|---|---|---|---|
| T1.1 Scaffold and shared types | `tui/package.json`, `tui/bun.lock`, `tui/tsconfig.json`, `tui/bunfig.toml`, `tui/.gitignore`, `tui/bin/wingtip`, `tui/src/state/{model,actions,ids}.ts`, `tui/src/transport/{types,errors}.ts`, `tui/src/voice/types.ts`, `tui/src/controller/types.ts`, `tui/src/config/{types,secret}.ts`, `tui/test/setup.ts`, `tui/test/helpers/{tmp,fake-spawn}.ts` | none | exact pins (3.1); the type modules of 3.3, 3.4, 3.7, 5.2, `ComposerHandle` (3.10.5), `StateFile` and `TokenSource` (3.11); `Secret`; preload; `tsc` passes on stubs |
| T1.2 State core | `tui/src/state/{reducer,correlate,turn-text,layout,store,coalesce,approval-measure}.ts`, `tui/test/state/{turn-text,reducer,layout,approval-measure}.test.ts` | T1.1 | 3.3 store, 3.5 sealing including rule 3b, 3.12.1 layout, approval measurement |
| T1.3 Transport core and gateway | `tui/src/transport/{guards,event-channel,framing,sse,backoff,listener-owner,gateway,gateway-wire,control-acks}.ts`, `tui/test/transport/{framing,sse,gateway-wire,listener-owner,backoff,guards}.test.ts`, `tui/test/integration/gateway-transport.test.ts`, `tui/test/mocks/mock-gateway.ts`, `tui/test/fixtures/proc-net/*` | T1.1 | `GatewayTransport` (3.8, 3.8.1) in legacy and current modes; the mock gateway |
| T1.4 Config and security | `tui/src/config/{config,flags,token-file,paths,tenant-access,tenant-select,state-file,instance-lock,child-env,proxy-env}.ts`, `tui/src/sanitize.ts`, `tui/src/log.ts`, `tui/test/config/{config,tenant-select,instance-lock,proxy-env}.test.ts`, `tui/test/fixtures/proxy-child.ts`, `tui/test/sanitize.test.ts` | T1.1 | 3.11 including socket path resolution (`defaultSocketPath()` from `v4:tui/src/repl-client.ts:36-50`) |
| T1.5 Images | `tui/src/images/{sniff,limits,attach,clipboard,paste-paths,screenshot,protocol,data-url,export}.ts`, `tui/src/components/attachment-bar.tsx`, `tui/test/images/*.test.ts`, `tui/test/mocks/fake-clipboard.ts` | T1.1, T1.4 (`child-env.ts`) | section 6 client side |
| T1.6 Voice core and LunarVoice | `tui/src/voice/{errors,manager,ptt,recorder,recorder-opentui,recorder-parec,wav,levels,vad}.ts`, `tui/src/voice/sources/{index,lunarvoice}.ts`, `tui/test/voice/{wav,levels,vad,ptt,queue,fallback}.test.ts`, `tui/test/mocks/{fake-recorder,fake-sources}.ts` | T1.1, T1.4 (`child-env.ts`) | 5.1, 5.3.1, 5.4, 5.5, 5.7, the section 7 state machine |
| T1.7 App shell, controllers, components | `tui/src/{main.tsx,app.tsx,util.ts}`, `tui/src/controller/{supervisor,session,commands,keymap,composer-handle,credential-input,approval-reply,panel-content,footer-hint,copy,notifier}.ts`, `tui/src/components/{transcript,turn-view,header,composer,mic-widget,vu-meter,approval-panel,notices,footer,info-panel,tenant-picker}.tsx`, `tui/src/components/theme.ts`, `tui/test/ui/*.test.tsx`, `tui/test/commands/{slash,approval-reply}.test.ts`, `tui/test/integration/{gateway-session,gateway-reconnect}.test.ts`, `tui/test/helpers/{render.tsx,frames.ts}`, `tui/test/mocks/fake-transport.ts` | T1.2, T1.3, T1.4, T1.5, T1.6 | the runnable TUI: 3.1 renderer, 3.6, 3.10, 3.12, 7.4 |
| T1.8 `tui-access` | `ic/scripts/mt-admin-lib/16-tui-access.sh` (NEW), `ic/scripts/lunarwing-mt-admin.sh`, `ic/scripts/tests/test-mt-admin-tui-access.sh` (NEW), `docs/guides/MT-ADMIN-QUICKSTART.md`, `docs/ops/MULTITENANCY-PRODUCTION.md`, `docs/ops/TENANT-CONFIGURATION.md` | none | section 9 and 11.4 |
| T1.9 CI lane and README | `.gitlab/ci/validate.yml`, `.gitlab/scripts/validate_config.py`, `.gitlab/README.md`, `tui/README.md` | T1.1 (package scripts) | the pinned `tui-ts` lane (11.3.6); the README of 12 |

### M1b

| Task | Owns (files) | Deps | Deliverable |
|---|---|---|---|
| T1.10 Hex sources | `tui/src/voice/sources/{hex-locate,hex-http,hex-app,hex-embedded}.ts` (NEW), `tui/src/voice/sources/index.ts`, `tui/test/voice/hex-locate.test.ts`, `tui/test/integration/hex.test.ts`, `tui/test/mocks/mock-hex.ts`, `tui/README.md` | M1a (T1.3 `listener-owner.ts`, T1.6 `VoiceSource` contract) | 5.3.2, 5.3.3 |

### M2a

| Task | Owns (files) | Deps | Deliverable |
|---|---|---|---|
| T2.1 Lifecycle and channel primitives | `ic/crates/lunarwing_common/src/client_caps.rs` (NEW), `ic/crates/lunarwing_common/src/lib.rs`, `ic/src/channels/lifecycle.rs` (NEW), `ic/src/channels/channel.rs`, `ic/src/channels/mod.rs`, `ic/src/channels/manager.rs`, `ic/src/channels/image_mime.rs` (NEW), `ic/src/channels/wasm/wrapper.rs`, `ic/tests/support/test_channel.rs`, `ic/tests/support/test_rig.rs` | none | 4.1, 8.2.1, the 6.2 server helper; `TestChannel::captured_lifecycle()` and the `TestRig` pass-through |
| T2.2 Agent loop wiring, quit refusal, DEC-1 guard | `ic/src/agent/agent_loop.rs`, `ic/src/agent/commands.rs`, `ic/src/agent/dispatch.rs`, `ic/tests/e2e_turn_lifecycle.rs` (NEW), `ic/tests/e2e_quit_refusal.rs` (NEW), `ic/tests/fixtures/llm_traces/tui/silent_reply.json` (NEW) | T2.1 | 8.2.2, 8.4 |
| T2.3 V2 bridge: hints, correlation, `ImageGenerated` | `ic/src/bridge/router.rs`, `ic/src/bridge/router/outcome.rs`, `ic/src/bridge/router/outcome/tests.rs`, `ic/src/bridge/effect_adapter.rs`, `ic/src/bridge/generated_images.rs` (NEW), `ic/src/bridge/mod.rs`, `ic/src/tools/builtin/image_gen.rs`, `ic/src/agent/dispatcher.rs` | T2.1; T2.4 (`SseManager::broadcast_correlated`) | 8.2.3, 8.3.1 (router part), 8.3.6 |
| T2.4 Gateway web contract | `ic/src/channels/web/{sse,types,mod,server,ws,voice}.rs`, `ic/src/channels/web/tests/correlation.rs` (NEW), `ic/src/channels/web/tests/mod.rs`, `ic/tests/voice_gateway_integration.rs` | T2.1 | 8.3.1-8.3.4 including credential continuations and `subscribe_until`, 8.3.7, the 8.2.4 gateway mapping |
| T2.5 M2a docs | `docs/guides/GATEWAY_CHAT_API.md` (NEW), `docs/guides/GATEWAY_VOICE_API.md`, `ic/src/NETWORK_SECURITY.md`, `docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md` | T2.2, T2.3, T2.4 | the section 12 rows marked M2a |

### M2b

| Task | Owns (files) | Deps | Deliverable |
|---|---|---|---|
| T2.6 Client-token store and middleware | `ic/src/channels/web/client_tokens.rs` (NEW), `ic/src/channels/web/auth.rs`, `ic/src/channels/web/mod.rs`, `ic/src/main.rs` | M2a | 8.3.5: store, path policy, reloader, `credential_revoked`, middleware branch, `with_client_tokens`, the `mod.rs:98` literal, the `store_from_env` call in `main.rs` |
| T2.7 Client-token consumers in the handlers | `ic/src/channels/web/server.rs` | T2.6 (`ClientCredential`, `ClientTokenStore::is_active`, `credential_revoked`) | the 8.3.5 `server.rs` bullets: stream revocation, capabilities `credential`, the `auth-token` pending rule with `session_has_pending_auth` |
| T2.8 M2b docs | `docs/guides/GATEWAY_CHAT_API.md`, `docs/guides/GATEWAY_VOICE_API.md`, `ic/src/NETWORK_SECURITY.md`, `docs/ops/TENANT-CONFIGURATION.md` | T2.6, T2.7 | the section 12 rows marked M2b |

### M3

| Task | Owns (files) | Deps | Deliverable |
|---|---|---|---|
| T3.1 Shared wire module and fixtures | `ic/crates/lunarwing_common/src/repl_wire.rs` (NEW), `ic/crates/lunarwing_common/src/lib.rs`, `ic/crates/lunarwing_common/tests/fixtures/replv3/{v1_frames,client_frames,server_frames}.ndjson` (NEW) | M2a (`client_caps`) | 4.3.2 types, encoders, redacting `Debug`, contract tests |
| T3.2 Socket channel on protocol 1 | `ic/src/channels/unix_socket_repl/{mod,listener,conn,writer,registry,outbound,tests}.rs` (NEW); deletes `ic/src/channels/unix_socket_repl.rs` and `ic/src/unix_socket_repl.rs` | T3.1 | 8.5.1-8.5.5 |
| T3.3 `ChannelManager` lock release | `ic/src/channels/manager.rs` | none | 8.5.8 |
| T3.4 `mount_guard` socket protection | `ic/src/sandbox/mount_guard.rs` | none | 8.5.7 |
| T3.5 Legacy-client pins, owner check, CI, M3 docs | `ic/src/cli/repl.rs`, `ic/unix-socket-client/src/lib.rs` (tests only), `ic/unix-socket-client/README.md`, `ic/src/NETWORK_SECURITY.md`, `.gitlab/ci/rust.yml` | T3.1 | 8.5.6 fixture tests; the `cli/repl.rs` owner check; the `rust-test-lib` line `bash .gitlab/scripts/cargo-ci.sh test -j2 --locked --manifest-path unix-socket-client/Cargo.toml -- --test-threads=1` (F11); the section 12 rows marked M3 |

### M4

| Task | Owns (files) | Deps | Deliverable |
|---|---|---|---|
| T4.1 REPLv3 channel | `ic/src/channels/unix_socket_repl/{mod,conn,writer,registry,inbound,outbound,transcribe,tests}.rs` (`inbound.rs`, `transcribe.rs` NEW), `ic/crates/lunarwing_common/src/repl_wire.rs`, `ic/crates/lunarwing_common/tests/fixtures/replv3/*.ndjson`, `ic/src/agent/submission.rs` | M3; M2a (`status_sse_event`, `VoiceApi::transcribe_b64`, `decode_images`) | 4.3 server behaviour, 8.6, the parser debug line |
| T4.2 Voice wiring | `ic/src/main.rs` | T4.1 (`with_voice`) | 8.6.3 wiring |
| T4.3 REPLv3 end-to-end tests | `ic/tests/e2e_unix_socket_repl.rs` (NEW), `ic/tests/fixtures/llm_traces/replv3/approval_round_trip.json` (NEW) | T4.1 | the 11.1 M4 e2e table |
| T4.4 Client socket transport | `tui/src/transport/{socket,replv3-wire,legacy-wire}.ts` (NEW), `tui/src/main.tsx`, `tui/src/config/config.ts`, `tui/src/controller/commands.ts`, `tui/test/transport/{replv3-wire,replv3-golden}.test.ts`, `tui/test/integration/socket.test.ts`, `tui/test/mocks/mock-replv3-daemon.ts` | T4.1 (final fixtures) | 3.9, `/thread resume`, the 11.3.3 socket and golden tests |
| T4.5 M4 docs | `docs/specs/REPLV3_PROTOCOL.md` (NEW), `tui/README.md`, `docs/guides/GATEWAY_VOICE_API.md`, `docs/guides/MT-ADMIN-QUICKSTART.md` | T4.1, T4.4 | the section 12 rows marked M4 |

---

## 15. Risks and rollback

| Risk | Impact | Mitigation | Rollback |
|---|---|---|---|
| Lifecycle hook in `agent_loop.rs` (high-risk file) violates exactly-once or delays delivery | duplicate or missing `turn_finished`; clients stall or double-seal | additive trait method with a no-op default; mechanical call sites; e2e `lifecycle_finished_exactly_once_per_message`; client ignores duplicates | revert M2a; clients fall back to legacy mode on 404 capabilities |
| Outcome-hint race for `detached` (late watcher finishing before the agent loop reports) | a `detached` after a terminal outcome | late watcher polls at 100 ms; client ignores `detached` for sealed turns | none needed |
| DEC-1 declined | model output (V1 `NO_REPLY`, V2 `Completed{response:None}`) can still stop the daemon over both TUI channels; on OpenRC more than 5 stops in 60 s exceed `respawn_max` (`08-units-openrc.sh:918-919`) | docs say so (8.4); the producers are tracked separately (DEC-6) | n/a |
| Listener-owner check (3.8.1) parses `/proc/net/tcp` | a wrong `tenant_uid` (tenant re-created under a new uid) or an unusual address form refuses a healthy gateway | fixtures in this host's format; the error names the port, the owner and the expected uid; re-running `tui-access` rewrites `tenant_uid` | the check is client-only; a revert restores the previous behaviour |
| Residual check-to-connect race on loopback ports | a port squatter that wins the race between the check and the connect receives one request | the window is one `readFile` plus one connect; reconnects are backed off | n/a (documented) |
| Bun proxy handling differs between versions (`delete` and `proxy:false` ineffective on 1.3.14) | the token could reach a proxy after a Bun upgrade | `neutralizeProxyEnv` assigns `""` and `NO_PROXY="*"`; the child-process test fails CI if a Bun version changes the behaviour | pin `CI_BUN_IMAGE`; engines field |
| OpenTUI textarea behaviour in tmux (Alt+Enter, Ctrl+J, Ctrl+_ parsing) | newline or undo keys do nothing inside tmux | `composer.test.tsx` feeds raw bytes (`\n`, `\x1b\r`, `\x1f`); supervised 16.3 | Enter and paste still work; bindings are data in `COMPOSER_KEY_BINDINGS` |
| Wayland clipboard ownership | text copied through the native host clipboard may vanish when the TUI exits unless a clipboard manager keeps it | supervised 16.4 checks paste after quitting; `wl-copy` (which forks a server) is the fallback | `/copy` only; disable copy-on-select |
| Desktop notifications | `notify-send` missing, or noisy | silent when missing; at most one per 10 s; `[ui] notify = "off"` | config |
| Transcription queue and retained clip | memory for up to three queued clips and one retained clip (120 s × 32 KB/s ≈ 3.8 MB each, about 15 MB worst case) | bounded queue; retained clip expires after 10 min; zero-filled | n/a |
| `ChannelManager` lock release changes timing for every channel | a `respond` may reach a channel instance just replaced by `hot_add` | tests with a gated channel; replaced instances fail or no-op instead of blocking | revert T3.3 alone |
| `mount_guard` socket protection | sandboxed tools lose IPC such as git's `fsmonitor--daemon.ipc` (git falls back to scanning); workdirs at or inside `$XDG_RUNTIME_DIR`, `/tmp/.X11-unix` or the tmux directory, and the roots `/`, `/run`, `/var/run`, are refused | intended; errors name the protected path; the relay socket and mask artifacts are mounted directly (`cli_runner.rs:419-471`) | revert T3.4 |
| Bind-mounting the empty mask file over a socket inode is untested against crun/runc | a run with a socket in the workspace fails | the failure is closed and visible; supervised 16.13 | revert the socket part of T3.4 |
| Mask scan truncation (DEC-4) | a socket deeper than 5 levels or after 10 000 entries in a large ordinary tree stays reachable | the known socket directories are protected structurally; truncation is logged | n/a (documented) |
| Strict socket parent policy | a socket configured in a shared directory (for example `LUNARWING_SOCKET=/tmp/lw.sock`) is refused at start | the error names the directory and its mode; the default paths pass | revert T3.2's parent check |
| Not-serving health mode | a socket that never started is reported healthy | `error!` at start and one `warn!` at health check; avoids hourly watchdog restarts | revert to failing `start()` |
| G5 rejects images the web UI used to drop silently | a web upload with an SVG or bad base64 now fails with 422 | the web UI's own caps already match; message names the image | revert the `decode_images` call in `server.rs` |
| Client-token store bug | TUI auth fails, or (worse) accepts a revoked token | fail-closed parsing, constant-time compare, revocation, rotation and expiry tests, 2 s reload | `GATEWAY_CLIENT_TOKENS_FILE=none` disables client tokens without a rebuild; users re-grant with `--use-master-token` |
| V1 gateways clear auth mode only on the session's active thread (`server.rs:1876-1886`) | after `auth-token` succeeds, a TUI thread that is not the active thread may consume its next message as a credential (the text is not leaked, the message is lost) | known limitation, documented in the README; out of scope | n/a |
| Socket thread scoping changes v3 UX | each window is its own conversation; history of non-UUID scopes does not survive restarts | `--thread last` and `/thread resume`; documented | v1 behaviour unchanged |
| Global scheduler exclusivity | a socket TUI turn blocks XMPP and every other non-gateway channel | documented; the gateway is the default transport | n/a |
| OpenTUI 0.8.1 is days old and links a decode-only FFmpeg | rendering or clipboard regressions | exact pin, frozen lockfile, supervised smoke (16.3, 16.4) | 0.6.8 with `wl-paste` forced |
| Hex churn (6 releases in about 64 h; docs contradict source) | Hex sources break | Hex sources optional and probed; mocks follow 54a3a66 and the released 3.3.0 strings match; 16.9 against the release asset | `/voice use lunarvoice`; remove Hex from `voice.order` |
| Kimi K3 may ignore images | images "sent but not seen" | supervised 16.5 in M1a acceptance; first-send notice; README caveat | route images or extend enrichment (A.19) |
| Tenant gateways listen on 0.0.0.0 | the bearer token is the only barrier on LAN and NetBird | loopback URLs from `tui-access`; cleartext to non-loopback refused; listener-owner check | separate rebind change (DEC-5) |
| Release builds for dev daemons conflict with AGENTS.md | M3/M4 supervised tests blocked | ask first (DEC-2); automated tests need no live daemon | n/a |
| CI Bun 1.4.4 versus host 1.3.14 | test drift | engines `>=1.3.14`; run both | pin `CI_BUN_IMAGE` |
| SSH-agent socket uses the same exists-unlink-bind pattern (`ic/src/bridge/ssh_agent.rs:131-137`) | a second daemon could take over the agent socket path | out of scope (different listener, workers need 0666); noted for a follow-up | n/a |

Rollback summary: M1a and M1b are client and shell only (revert, and `tui-access --revoke` removes files). M2a, M2b, M3 and M4 are separate MRs; reverting any of them leaves older daemons that the client detects (capabilities 404, `credential:"master"`, v1 welcome) and serves in degraded mode. Protocol 1 bytes are pinned by fixtures, so legacy clients are unaffected by any revert.

---

## 16. Items that need a supervised live test

Each needs Christopher present (microphone, desktop session, root, or tenant upgrades). None of them is automated.

| # | Item | Steps | Pass criteria |
|---|---|---|---|
| 16.1 | Live tenant flags (before M1a testing) | as root: `lunarwing-mt-admin.sh status <t>`, then read (without printing secrets) `ENGINE_V2`, `ENGINE_V2_CHANNELS`, `TRANSCRIPTION_PROVIDER`, `TRANSCRIPTION_ENABLED`, presence of `VOICE_AUTH_TOKEN`, `GATEWAY_HOST` through mt-admin's tenant-side reader; check the LunarVoice sidecar image against HEAD (A.14) | flags recorded per tenant; decides whether streaming and V2 images are expected and whether voice is configured |
| 16.2 | Microphone capture | kitty on Hyprland: `/voice test` with OpenTUI capture at 16 kHz on the default PipeWire source; repeat with `parec` forced; check `pactl list source-outputs` while idle and while recording; select the C922 with `/voice device` and relaunch | levels move (the muted C922 shows the no-signal warning); no source-output while idle although the engine exists; "opening mic..." then `REC` appear; time to first chunk noted; the device choice survives the relaunch; quit leaves no `parec` |
| 16.3 | Push-to-talk and composer keys | kitty inside tmux and kitty direct: F9 tap, F9 hold (repeat inference with `repeat_delay_ms = 300`), Ctrl+Space (NUL in tmux), mouse hold on the widget released elsewhere, KKP release in kitty direct, focus-out during a hold, a second dictation while the first transcribes; Ctrl+J and Alt+Enter newlines and Ctrl+- undo inside tmux | every mode starts and stops as in 7.3 and shows its status line; no stuck recordings; transcripts insert in order; the composer keys work in both environments |
| 16.4 | Clipboard, screenshots and copy | Ctrl+V of a PNG and a JPEG copied from a browser and a file manager with OpenTUI 0.8.1; `wl-paste` fallback forced; `/shot` region and `/shot full` (output picked with `slurp -o`); drag-and-drop from a file manager; drag-select reply text, quit the TUI, paste into another application | no desktop freeze (#1825); images attach with correct sniffed types; a full capture fits the limit (JPEG fallback when needed); the cliphist and storage notices show; the copied text is still pasteable after the TUI exits (else record that a clipboard manager is required) |
| 16.5 | Kimi K3 vision (M1a acceptance) | gateway TUI on one tenant: send a known test image ("what number is written here?") | the reply proves the model saw it; otherwise record the failure mode and decide A.19 |
| 16.6 | Gateway session end to end | M1a: `tui-access <t> --for-user sun --use-master-token`; one turn, one image, one approval (including a long-parameter approval through Alt+V and a dictated "Yes." with confirmation), one interrupt, one LunarVoice transcription, a busy or offline dictation and Alt+R; `tui-access --revoke` | legacy badge in M1a; every action completes; notices name the voice outcome; after revoke the TUI goes offline with the `tui-access` hint |
| 16.7 | Client tokens on a tenant (after M2b) | `upgrade-tenant <t>`; `tui-access <t> --for-user sun`; TUI in exact mode; `curl` with the client token to `/api/settings` (expect 403) and to `POST /api/chat/auth-token` with nothing pending (expect 403); re-grant (rotation) and `--revoke` while the TUI is connected | 403 outside scope and for an unrequested credential; the SSE stream ends within about 2 s of rotation and of revocation, and the TUI continues after the rotation; web UI unaffected |
| 16.8 | Dev daemon over the socket (needs DEC-2) | release build in tmux; harness `start-lunarwing`; `lunarwing repl` and the TUI (M4) concurrently; two TUI windows on one profile; approval prompt; `/quit` from both; start a second daemon on the same socket path; restart the daemon | prompts appear on both clients; the two TUI windows get different threads; `/quit` refused; the second start reports "already serving" and the first keeps serving; the TUI resumes its thread after the restart |
| 16.9 | Hex (opt-in, M1b) | per `tui/README.md`: verify the `hex-3.3.0-linux-x64` binary or RPM checksum against the GitHub asset digest; `hex model install parakeet_unified_en --language en` (about 731 MB); then `/voice use hex-embedded`, and `/voice use hex-app` with the app running | ready line accepted; `GET /models` lists a verified model; CPU transcription succeeds (GPU is full); quitting the TUI ends the embedded child; tested version recorded |
| 16.10 | Web UI regression after M2a | web UI: send, image upload (PNG, then an SVG expecting a visible error), approval, interrupt | unknown SSE event names ignored; behaviour otherwise unchanged |
| 16.11 | Image rendering (M1a on a V1 tenant if one exists, M2a on V2) | kitty direct with `--image-protocol auto` and `kitty`; tmux with `auto` (blocks); `/image open` and `/image save` on a generated image | generated images render; tmux shows blocks; forcing kitty inside tmux shows the known misplacement warning; the full image opens in the desktop viewer and the saved file is 0600 |
| 16.12 | Desktop notification | focus another tmux window, or another Hyprland window with kitty direct; let an approval arrive | swaync shows "approval needed: <tool>" for the tenant without parameters; `notify = "off"` silences it |
| 16.13 | Sandbox socket protection (M3) | on a dev daemon: a shell tool call with `workdir` set to `$XDG_RUNTIME_DIR`, to `/`, and to a temp directory holding a bound Unix socket | the first two are refused with the protected-path message; in the third, the socket path shows an empty read-only file and `connect()` fails |
| 16.14 | Listener-owner check (M1a) | copy a tenant file, set `tenant_uid` to a wrong uid, launch with `--tenant` on the copy | the TUI goes offline with "port N is held by uid U, expected X" before any authenticated request (the tenant log shows no request from it) |

---

## Appendix A. Decisions and open questions for Christopher

### A.0 Decisions that need Christopher's input

| # | Decision | Options | Recommendation |
|---|---|---|---|
| DEC-1 | Guard the shutdown decision for the TUI channels (SEC-06): in `deliver_message_result`, `Ok(None)` from `gateway` or `unix_socket_repl` continues instead of stopping the daemon. This changes the effect of the two tracked bugs (V1 `NO_REPLY`, V2 `Completed{response:None}`) for these two channels only; their producers stay untouched and tracked separately. | (a) adopt the guard in T2.2; (b) leave the tracked bugs fully alone and document that model output can stop the daemon over both TUI channels | (a): a few lines and two e2e tests close a model-triggerable denial of service on the TUI path that the brief asks not to leave open |
| DEC-2 | DECIDED 2026-11-03: no dev-daemon release build. Live daemon tests (M3/M4 supervised tests, 16.8) run against a new test tenant on this host, built from the pushed `tuitui3` branch through mt-admin (`add-tenant`, then `upgrade-tenant <name> --target tuitui3`); socket tests run as that tenant's uid. This host is Fedora 43 + systemd; up to 24 cores allowed (`taskset -c 0-23`, `-j24`). | (b) | (b) |
| DEC-3 | Image persistence (SEC-09, A.20): every image sent from the TUI is stored base64 in the tenant's workspace with no expiry and is readable from every channel | (a) keep storing and disclose (this design); (b) skip `store_attachments` for TUI image turns through a metadata flag checked in `prepare_message`; (c) make it a per-profile option | (a) now: V1 image parts are visible only in their own turn (6.5), so the store is how later turns refer back; revisit (b) or (c) after M2a |
| DEC-4 | Sandbox mask-scan truncation (SEC-02): refuse a mount whose socket scan was truncated, or only warn | (a) warn and rely on the protected socket directories (this design); (b) refuse truncated mounts; (c) raise the caps | (a): (b) fails every default sandboxed command on tenants, whose workdir `<root>/ic` exceeds both caps; (c) only moves the boundary and slows every shell call |
| DEC-5 | DECIDED 2026-11-03: keep tenant gateways on `0.0.0.0`. The host firewall blocks the gateway ports on the LAN; NetBird is the remote access path. No rebind change. | (b) keep 0.0.0.0 | (b) |
| DEC-6 | DECIDED 2026-11-03: filed as WI#550 (both producers: V1 `NO_REPLY`, which the group-chat prompt instructs, and V2 `Completed{response:None}`); fix separately from the TUI work. | (a) | (a) |

### A.1 Open questions with proposed defaults

| # | Question | Proposed default |
|---|---|---|
| A.1 | Default gateway thread: `last` (a TUI-owned thread reused across launches), `new` per launch, or the shared `assistant` thread? | `last` (a second window of the same profile uses a new thread, 3.11.5) |
| A.2 | Tap `/dictations/{id}/audio` for a fallback copy? | moot until Hex dictations are scheduled (13, Deferred) |
| A.3 | (now DEC-2) | |
| A.4 | Delete the uncompiled orphan `ic/src/unix_socket_repl.rs` in M3? | yes |
| A.5 | Client-token default expiry: none, or 90 days? | none; docs recommend 90 days for laptops |
| A.6 | (now DEC-5) | |
| A.7 | Schedule the socket Engine V2 opt-in after M2a (then `allows_parallel_dispatch` for scoped socket threads too)? | not scheduled |
| A.8 | Keep `voice.lunarvoice_profile` (gateway voice from a socket session) in M4? | yes, optional |
| A.9 | Implement `broadcast()` to REPLv3 sessions (heartbeat, routine, self-repair notices) in M4? | yes (about 15 lines) |
| A.10 | Confirm exclusions: momentary-KKP-through-tmux passthrough, TTS playback, auto-send after dictation. (The multi-line composer is now in scope, UX-01.) | excluded |
| A.11 | Extend the `/quit` refusal to an allow-list (only the in-process `repl`), which would also cover XMPP, Signal, HTTP and WASM channels? | not in this work |
| A.12 | (now DEC-6) | |
| A.13 | G5 makes invalid web images fail with 422 instead of being dropped; acceptable? | yes |
| A.14 | Rebuild the LunarVoice sidecar image (the deployed image predates HEAD) before voice testing? | yes, via mt-admin |
| A.15 | Per-image limit 5 MiB (HTTP channel and attachment-store parity) or 8 MiB (OpenTUI clipboard cap)? | 5 MiB |
| A.16 | `lunarwing repl` stays a protocol 1 client? | yes, for now |
| A.17 | PTT defaults F9 + Ctrl+Space, with `repeat_delay_ms = 300` on this host; toggle auto-stop off by default? | yes |
| A.18 | R16 resolves the client-token file path in `client_tokens::store_from_env()` instead of a new `GatewayConfig` field; acceptable? | yes |
| A.19 | If Kimi K3 ignores images (16.5): route image turns to a vision-capable TensorZero function, or extend LunarVision enrichment beyond XMPP? | decide after 16.5, before M1a closes |
| A.20 | (now DEC-3) | |

---

## Appendix B. Review disposition

Every finding of the three reviews, with what this document does about it. "Accepted" means the proposed fix is in the design as written; "modified" means the problem is fixed differently or partly, with the reason; "deferred" means the affected component moved out of this release with the requirement recorded.

| ID | Disposition | Where | Notes |
|---|---|---|---|
| SEC-01 | accepted, scoped | 3.8.1, 3.6.5, 5.3.2, 5.3.3, 9.3, 9.5, 11.3, 11.4 (20, 23) | Verified: `/proc/net/tcp` shows each tenant gateway on `0.0.0.0:<port>` under its own uid (derpy 1010 on 10000, brightdawn 1009, spitfire 1011, fleetfoot 1017, dwarfstar 1008); the unit restarts after 5 s (`07-units-systemd.sh:1049-1050`). Scoping: tenant profiles are always checked (tenant gateways are local by definition); other loopback URLs use `expected_uid` or the current uid; other URLs are unchecked with a warning. The check runs before every token-bearing request, not only on connect. |
| SEC-02 | modified | 8.5.7, 10.3, DEC-4 | Accepted: protected runtime directories (plus `/tmp/.X11-unix` and the tmux directory), refusal of `/`, `/run`, `/var/run` as roots, `MaskScan.truncated`, the tests. Rejected: refusing the mount when the scan is truncated, because the default tenant workdir `<root>/ic` (`06-env-generation.sh:1615-1618`) exceeds both caps and every default sandboxed command would fail; truncation is logged and the residual risk is DEC-4. `masked_files_for_mount` keeps its signature, so `cli_runner.rs` is untouched. |
| SEC-03 | accepted, extended | 3.6.4, 3.6.1, 3.10.4, 8.6.1, 10.1, 10.3 | Verified `agent_loop.rs:1966-1971`, `session.rs:141`, `submission.rs:17`. Beyond documenting the parser's debug log, M4 reduces that line to the length (test `parse_debug_log_omits_content`). |
| SEC-04 | accepted | 3.6.2, 4.3.2, 4.3.6, 8.5.5, 8.6.2, 11 | Verified the gateway sends parameters untruncated (`mod.rs:504-516`) and V2 redacts only (`router.rs:240-242`). The v1 lockstep prompt keeps an 8 KiB cap but now says how much is hidden. |
| SEC-05 | accepted | 8.3.5, 9.4, 11.1 | `ClientCredential` carries digest and expiry; `is_active(&cred)`; `credential_revoked` adds the expiry timer; failed reloads bump the watch and deactivate every credential. |
| SEC-06 | accepted, pending DEC-1 | 8.4, 8.2.2, 10.1, 10.4, 11.1 | Verified: `is_silent_reply` runs on every V1 reply, not only group chats (`agent_loop.rs:2181-2186`); V2 `Completed{response:None}` returns `None` (`router.rs:155`). The guard sits at the shutdown decision; producers untouched; the alternative text applies if DEC-1 is declined. |
| SEC-07 | accepted | 4.2.1, 4.2.7, 8.3.5, 10.2, 11.1 | Verified that `chat_auth_token_handler` configures without a pending request (`server.rs:1625-1645`, `:1739-1777`) and that `configure_token` overwrites (`ic/src/extensions/manager.rs:6124`). |
| SEC-08 | modified (merged with UX-07) | 3.6.2, 3.10.3, 11.3 | With a prompt pending, keywords are never sent as chat and typed text never yields `always`, as proposed; the freshness rules are as proposed. Instead of never converting typed keywords, approve and deny ask for a confirmation that names the tool, so a dictated "Yes." can still answer (UX-07) while an accidental keyword cannot. |
| SEC-09 | modified | 5.6, 6.1, DEC-3 | Accepted the privacy row, the per-source first-use notice (`imageAck`), the attachment-bar caption and the open question (DEC-3). Not adopted: a second Enter for `/shot full`, because the `slurp -o` output pick of UX-08 is already a deliberate second step and the capture waits in the attachment bar until Enter. |
| SEC-10 | modified | 8.3.5, 9.3, 11.1, 11.4 (22) | Accepted (1) the protected-directory rule in the store and (2) the strict path in `tui-access`. Rejected (3) adding the file name to `PROTECTED_FILE_NAMES`: with (1) the daemon only reads the file inside a protected, never-mountable directory, copies elsewhere contain only SHA-256 digests of 256-bit random tokens, and the list is for credential-bearing files. |
| SEC-11 | modified | 8.5.2, 8.5.4, 10.3, 11.1 | Accepted the lock (`File::try_lock`, stable since 1.89; `TryLockError` present in the 1.96.1 std), the strict parent rule (checked against every layout in use), `Existing::Foreign` and the tests. Part 4 is limited to `cli/repl.rs`: the standalone `unix-socket-client` depends only on tokio, anyhow, dirs, serde and serde_json, and std has no `getuid`, so an owner check there needs a new direct dependency; its README documents the gap. |
| SEC-12 | deferred | 13 (Deferred) | The CLI source is deferred (UX-09); the crash-lease requirements are recorded for when it is scheduled. |
| SEC-13 | accepted | 9.2, 9.3, 11.4 (18, 21) | jq `test()` validation; `<invalid entry #n>`. |
| SEC-14 | modified | 3.11.4, 3.1, 10.2, 11.3 | Loopback probes on Bun 1.3.14 showed that deleting the variables, `proxy:false` and `proxy:""` all still proxy, while assigning `""` or `NO_PROXY="*"` works. The fix assigns instead of deleting, and the test runs a child process with an inherited `HTTP_PROXY`. |
| F1 | modified | 4.2.7, 8.3.1, 3.6.4, 11.1, 11.3 | Accepted `client_turn_id` on `auth-token` and `auth-cancel` and the stamped resume message. No new server event for cancel: V2 already broadcasts `gate_resolved{cancelled}` (`router/auth.rs:102-121`) and the client seals the owning turn on the 2xx. |
| F2 | accepted | 3.5 rule 3b, 3.6.1, 4.3.7, 8.6.2, 11 | Verified `router.rs:2526-2543`, `:3268-3285`, `thread_ops.rs:1133-1149`. The control-ack list gains the verified `/clear` and `/new` strings. |
| F3 | accepted | 3.9, 3.11.5, 4.3.3, 4.3.7, 8.6.2, 11 | Per-process token, `--thread last` and `/thread resume` for deliberate reuse, `thread_in_use`, and a profile instance lock (Bun has no `flock`, so an `O_EXCL` lock file with the pid). |
| F4 | accepted | 11 (rule), 11.2, 13 | Verified `check_no_panics.py:15-25` and the existing workaround at `outcome/tests.rs:101-102`. |
| F5 | accepted by restructuring | 13, 14 | Client tokens moved to M2b, where T2.6 owns `web/mod.rs`; no M2a task needs `ClientTokenStore`, so there is no cycle. |
| F6 | accepted | 3.3, 5.2, 3.10.5, 3.8, 14 | Scaffold types moved to T1.1 (`voice/types.ts`, `state/model.ts` `Store`, `controller/types.ts`, `config/types.ts`); the gateway test split into `gateway-transport.test.ts` (T1.3) and `gateway-session.test.ts` (T1.7). |
| F7 | modified | 8.6.4, 8.6.1, 11.1 | Accepted the routing correction (`send_status` has metadata only, `channel.rs:512-518`; `ApprovalNeeded` carries no thread id, `:372-381`). Rejected the fail-open precheck: it would let a bare keyword approve a tool call whose prompt no client displayed; the user sees V1's "Waiting for approval: … Use /interrupt to cancel." and interrupts (test `internal_turn_approval_is_not_answerable_blind`). |
| F8 | accepted (first option) | 8.5.4, 10.3, 15 | Verified `lunarwing-watchdog.sh:85-104` and `manager.rs:343-363`. Non-restartable conditions report healthy and not serving; the watchdog scripts are unchanged. |
| F9 | accepted (second option) | 8.3.7 | `transcribe_b64` returns `SttTranscript` / `SttFailure`; `VoiceProxyError` and `SidecarTranscribeResponse` stay private. |
| F10 | accepted | 11.3.6, 14 (T1.9) | Verified `validate_config.py:1304-1361`, `:1478-1560` and that the lane tests synthesize from the tuple (`test_goal1_ci.py:2131-2144`). |
| F11 | accepted | 14 (T3.5), 11.2, 13 (M3) | `rust-test-lib` runs the standalone crate's tests; `cargo-ci.sh` changes into `ic/` first. |
| F12 | deferred | 13 (Deferred) | Both headers recorded for the deferred `hex-dictations` source. |
| F13 | accepted | 8.3.5 | `client_tokens::store_from_env`. |
| F14 | accepted | 12, 14 (T1.8, T2.8) | Verified `docs/ops/TENANT-CONFIGURATION.md:262-288`. |
| F15 | accepted | 13 | M2 split into M2a and M2b and re-estimated. |
| UX-01 | accepted | 3.10.4, 3.10.5, 3.12.1, I13, 0.4, A.10 | Verified in OpenTUI 0.8.1: the textarea bindings merge by name and modifiers, `handlePaste` keeps newlines, `replaceText` is undoable, and 0x1F parses as ctrl+`_`. |
| UX-02 | accepted | 5.4, 5.4.1, 5.5, 5.6 | Retained clip with Alt+R; the VAD no longer discards clips; local discard only for "too short" or "no signal". |
| UX-03 | accepted | 7.4, 5.4, 5.3.1, 3.7 | Labels of at most 8 columns with `wrapMode="none"`; the status line strings; outcome and end-of-chain notices; `lunarvoice` unavailable while offline; the image note. |
| UX-04 | accepted | 3.12.3, 3.10.4, 3.6.3, 3.6.1 | Info panel (absolute, `zIndex`), F1, footer table, Esc precedence, Ctrl+S on an empty composer interrupts only. |
| UX-05 | accepted | 6.5, 5.3.2, 13 (M1a, M1b, M2a), 16 | 16.5 and 16.11 in acceptance; first-send vision notice with an opt-out list; 16.9 as an opt-in M1b step against the release asset, whose strings match the mocks. |
| UX-06 | accepted, queue bounded | 5.4, 5.5, 7.2, 7.3, 3.11.2 | Background probes; engine at startup with `autoStart:false`; `capture_started` on the first chunk; toggle auto-stop off by default (3500 ms when on); at most three queued clips. |
| UX-07 | modified (merged with SEC-08) | 3.10.3 | Normalized matching as proposed; a match asks for a confirmation instead of approving at once. |
| UX-08 | accepted | 6.1 | Verified `grim -h` (`-s` defaults to the greatest output scale; `-t jpeg`, `-q`) and `slurp -h` (`-o`). |
| UX-09 | accepted | 5.1, 5.3.4, 5.4, 13 (M1a/M1b), 14 | Hex limited to `/transcriptions`; dictations and the CLI deferred; M1a is the checkpoint before Hex. |
| UX-10 | accepted | 3.11.5, 3.8, 3.11.6, 5.5 | Picker with `<select>`, `lastTenant`, 401 hint by token kind, persisted `voiceDevice`. The exit-2 message cannot list tenants because the client never reads the registry (R14). |
| UX-11 | accepted | 3.12.4, 3.10.1, 16.4 | Copy on selection end through the host clipboard, `wl-copy` fallback, OSC 52 outside tmux only; Wayland ownership on exit is a risk checked in 16.4. |
| UX-12 | accepted | 3.6.2 | Alt+V merged with SEC-04; `notify-send` with tenant and tool only, rate-limited. |
| UX-13 | accepted | 3.6.4 | Merged with SEC-03. |
| UX-14 | accepted | 6.4, 3.10.1 | `/image open` and `/image save` with `O_EXCL | O_NOFOLLOW` 0600 files. |
| UX-15 | accepted | 3.12.2, 3.11.2 | `NO_COLOR`, text labels for every state, `reduce_motion`, `cursor_blink`. |
