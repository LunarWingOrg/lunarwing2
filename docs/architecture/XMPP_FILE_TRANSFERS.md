# XMPP File Transfer Support

## Overview

LunarWing supports file transfers over XMPP in both directions, in DMs and group chats (including OMEMO-encrypted):

- **Outbound** (agent → user): XEP-0363 HTTP File Upload + OOB URL delivery (OMEMO-encrypted when the target is an encrypted room/DM).
- **Inbound** (user → agent): URL extraction from incoming stanzas — and from decrypted OMEMO bodies — with automatic download, including AES-256-GCM-encrypted `aesgcm://` media (XEP-0454).

The XMPP client also advertises its capabilities (XEP-0030/0115) and answers every IQ request, so contacts' clients recognize the agent as a valid recipient instead of refusing to send.

## Protocol Background

| XEP | Role |
|-----|------|
| **XEP-0363** (HTTP File Upload) | Sender requests an upload slot, PUTs the file, shares the GET URL. |
| **XEP-0066** (Out of Band Data) | The file URL travels in `<x xmlns='jabber:x:oob'><url>…</url></x>`. |
| **XEP-0454** (OMEMO Media Sharing) | Encrypted files use an `aesgcm://…#<hex(IV‖key)>` URL; the fetched body is AES-256-GCM ciphertext with the 16-byte tag appended. |
| **XEP-0030 / XEP-0115** (Service Discovery / Entity Caps) | The agent advertises an identity + feature set so clients know what it supports. |
| **XEP-0199** (Ping) | The agent answers pings to confirm liveness. |

Most clients (Conversations, Gajim, Dino) send the file URL as both the message body and an OOB element. Encrypted media usually arrives as an `aesgcm://` URL — sometimes only inside the (encrypted) body, with no cleartext OOB element.

## Capability Advertisement (XEP-0030 / XEP-0115)

The XMPP client (compiled into the bridge) answers incoming IQ stanzas rather than dropping them — required by RFC 6120 §8.2.3, and the reason clients will offer to send files at all:

- `disco#info` get → `<iq type='result'>` with identity `client/bot "LunarWing"` and features `http://jabber.org/protocol/disco#info`, `jabber:x:oob`, `urn:xmpp:ping`. `lunarwing_disco_info()` is the single source of truth.
- `urn:xmpp:ping` get → empty result.
- Any other get/set → `<error type='cancel'><service-unavailable/></error>` (no silent drops).
- Presence carries a XEP-0115 `<c/>` caps element whose `ver` is computed (`caps::compute_disco` + `caps::hash_caps`) from the same `disco#info`, so the advertised hash always matches the response.

Without this, capability-checking clients time out and treat the agent as unable to receive files (some then fall back to Jingle/XEP-0234, which the agent does not implement). Built in `build_iq_reply()` / `build_initial_presence()`.

## Architecture

```
Outbound (agent sends file):
  Agent tool produces OutboundAttachment
    -> XmppChannel.broadcast_with_attachments()
    -> XEP-0363 slot request (IQ to upload service)
    -> HTTP PUT file bytes to slot URL
    -> XMPP message with body=GET_URL + <x xmlns='jabber:x:oob'>
    -> OMEMO encryption if target is an encrypted room/DM

Inbound (user sends file):
  XMPP stanza arrives (OOB element and/or aesgcm:// URL in the body)
    -> XmppChannel.handle_message_stanza()   (OMEMO-decrypts the body first)
    -> extract_inbound_attachments(payloads, body)
         collect_oob_urls()    : <x jabber:x:oob> URLs (capped)
         collect_aesgcm_urls() : aesgcm:// URLs found only in the decrypted body
    -> bounded-concurrency download (<=4 at once), each via download_oob_file():
         http(s)   : GET + streaming size cap (read_capped_body)
         aesgcm:// : GET https form + AES-256-GCM decrypt (download_aesgcm_file)
    -> IncomingMessage.attachments populated with data + metadata
    -> Bridge enqueue_message() base64-encodes into BridgeMessage
    -> WASM channel on_poll() decodes, calls store_attachment_data()
    -> Host reconstructs IncomingAttachment for agent processing
    -> Supervised agent user-input task calls VisionEnrichmentMiddleware
         POST loopback /v1/vision/analyze with host-side Bearer auth
         store bounded OCR/VL result in attachment.extracted_text
    -> Legacy and Engine V2 context includes extracted text
    -> Original bytes still reach provider-native multimodal input
```

## Inbound File Transfer Details

All inbound logic lives in `ic/src/channels/xmpp/mod.rs`.

### URL extraction (`extract_inbound_attachments`)

1. `collect_oob_urls(payloads, MAX_OOB_ATTACHMENTS + 1)` — parses `<x xmlns='jabber:x:oob'>` elements (via `Oob::try_from`). The effective cap is all-or-nothing: a stanza with more than 10 URLs has its body cleared and **no** downloads attempted.
2. `collect_aesgcm_urls(body, …)` — scans the **decrypted body** for `aesgcm://` URLs and adds any not already present (deduped, same cap). This covers encrypted files whose client omits the cleartext OOB element. Plain `https://` links in a body are intentionally **not** auto-downloaded.
3. Downloads run with **bounded concurrency** (`buffered`, ≤4 at once) so one slow URL can't serialize the batch and stall the single client event loop. Results stay in stanza order; per-URL failures are logged and skipped.

### Download (`download_oob_file`)

- **The size cap is enforced while streaming** (`read_capped_body` over `response.bytes_stream()`): the body is read chunk-by-chunk and aborted the moment it exceeds 20 MB, so a missing or understated `Content-Length` cannot cause unbounded buffering. A `Content-Length` header above the cap is also rejected up front.
- MIME type is inferred from the HTTP `Content-Type` (http/https path) and the filename via `filename_from_url()`.

### Encrypted media (`aesgcm://`, XEP-0454)

- `download_aesgcm_file()` handles `aesgcm://` URLs. `parse_aesgcm_url()` splits the URL into its https fetch form and the `IV‖key` from the `#fragment`; the ciphertext is fetched (same streaming cap) and `decrypt_aesgcm()` AES-256-GCM-decrypts it. Both the standard 12-byte IV and the legacy 16-byte IV are supported.
- Because the server stores ciphertext, MIME is inferred from the URL filename (`mime_guess`) rather than `Content-Type`. The original `aesgcm://` URL is kept as `source_url` so body deduplication still matches.

### Filenames (`filename_from_url`)

The last URL path segment is used, with any query/fragment stripped. A missing extension is fine — an opaque XEP-0363 segment (e.g. a UUID) is still a unique, useful name, which keeps distinct files from colliding on the downstream `oob-{filename}` storage key.

### Body deduplication / empty-body handling

- Every recognized attachment URL (downloaded or not) is stripped from the message body before it reaches the model, even when embedded mid-sentence; the body ends up empty only when nothing else remains. Whole-body clearing applies when the URL count exceeds the cap.
- Messages with no text body but a valid attachment URL are accepted rather than dropped.

### Bridge transport

`BridgeMessage` carries attachments as `Vec<BridgeAttachment>` with base64-encoded file data. The field uses `#[serde(default)]` for backward compatibility with older bridge/channel versions. `enqueue_message()` drops attachment records whose in-memory `data` is empty rather than forwarding unusable payloads.

### WASM channel processing

The WASM XMPP channel (`ic/channels-src/xmpp/src/lib.rs`) decodes inbound
attachments during `on_poll()`: it base64-decodes each
`BridgeIncomingAttachment`, stages bytes via `store_attachment_data()`, and emits
`InboundAttachment` records that the host merges into `IncomingAttachment.data`.
The staging store permits 20 MiB per attachment and 50 MiB across the callback,
but that is not the final message budget: `emit_message()` independently applies
`MAX_ATTACHMENT_TOTAL_SIZE` and retains only the in-order attachment prefix whose
cumulative declared-or-actual bytes fit within 20 MiB. The effective aggregate
that reaches the agent is therefore 20 MiB, even though temporary callback
staging is allowed up to 50 MiB.

## Limits

| Limit | Value | Enforced at |
|-------|-------|-------------|
| OOB/aesgcm URLs processed per stanza | 10 (`MAX_OOB_ATTACHMENTS`) | XmppChannel |
| Concurrent inbound downloads | 4 (`MAX_CONCURRENT_OOB_DOWNLOADS`) | XmppChannel |
| Per-file download size (enforced while streaming) | 20 MB (`OOB_MAX_FILE_SIZE`) | XmppChannel |
| Download timeout | 30 seconds | XmppChannel (reqwest client) |
| Download DNS resolution timeout | 5 seconds (`OOB_DNS_TIMEOUT_SECS`) | XmppChannel (egress guard) |
| Attachment extraction timeout | 60 seconds (`OOB_EXTRACTION_TIMEOUT_SECS`) | XmppChannel |
| Per-attachment staging store | 20 MB | WASM host (`store_attachment_data`) |
| Total attachment staging per callback | 50 MB | WASM host (`store_attachment_data`) |
| Total attachments emitted to the agent | 20 MB cumulative in-order prefix | WASM host (`emit_message` / `validate_attachments`) |
| Decoded image size for LunarVision enrichment | 10 MB | VisionEnrichmentMiddleware + OCR sidecar |
| Concurrent LunarVision analyses | 2 | VisionEnrichmentMiddleware |
| LunarVision request timeout | 45 seconds | VisionEnrichmentMiddleware |
| LunarVision response / extracted text | 1 MB / 100,000 bytes | VisionEnrichmentMiddleware |
| Outbound attachment bytes read from disk per response | 50 MB (`MAX_TOTAL_ATTACHMENT_BYTES`) | WASM host (`read_attachments`) |
| `/v1/messages/send` JSON body (base64 attachments) | 70 MiB (`MAX_SEND_BODY_BYTES`), send route only — other bridge routes keep axum's 2 MiB default | xmpp-bridge (`DefaultBodyLimit`) |
| Upload PUT timeout | 120 seconds | XmppChannel (outbound) |

## Files

| Component | File |
|-----------|------|
| XmppChannel — IQ/disco responder, caps, extraction, download, aesgcm decrypt | `ic/src/channels/xmpp/mod.rs` |
| Bridge contract (BridgeMessage with attachments) | `ic/openclaw-ports/xmpp/bridge/src/lib.rs` |
| Bridge service (enqueue_message forwarding) | `ic/bridges/xmpp-bridge/src/main.rs` |
| WASM XMPP channel (decode + emit) | `ic/channels-src/xmpp/src/lib.rs` |
| WIT interface (InboundAttachment, store-attachment-data) | `ic/wit/channel.wit` |
| WASM host (attachment storage + validation) | `ic/src/channels/wasm/host.rs` |
| Automatic image OCR/VL enrichment | `ic/src/vision_enrichment.rs` |

## Supported Attachment Types

The WASM host enforces an explicit MIME-type prefix allowlist. Any MIME type
not matching one of these prefixes is rejected.

`AttachmentKind::from_mime_type()` determines the classification for accepted
types. The allowlist and the kind classification are independent decisions:
`image/*` maps to `Image`, `audio/*` maps to `Audio`, and every other allowed
prefix, including `video/*`, maps to `Document`.

| Prefix | Allowed | AttachmentKind |
|--------|---------|---------------|
| `image/` | yes | Image |
| `audio/` | yes | Audio |
| `video/` | yes | Document |
| `application/pdf` | yes | Document |
| `application/vnd.` | yes | Document |
| `application/msword` | yes | Document |
| `application/rtf` | yes | Document |
| `text/` | yes | Document |
| `application/json` | yes | Document |
| `application/zip` | yes | Document |
| `application/gzip` | yes | Document |
| `application/x-tar` | yes | Document |
| `application/octet-stream` | yes | Document |

All other MIME types are rejected outright.

## OMEMO Considerations

Encrypted file shares are handled end-to-end:

- **Unencrypted messages**: the OOB `<x>` element is present in `msg.payloads` and extracted directly; `https://` files are downloaded as-is.
- **Encrypted DMs/rooms**: the body is OMEMO-decrypted first, then scanned for `aesgcm://` URLs (`collect_aesgcm_urls`) — covering clients that omit the cleartext OOB element to avoid leaking the URL to the server. `aesgcm://` media is fetched and AES-256-GCM-decrypted locally (`download_aesgcm_file`). An `aesgcm://` URL carried in an OOB element is handled the same way.
- Plain `https://` URLs that appear only in a (decrypted) body are **not** auto-downloaded — only the explicit `aesgcm://` encrypted-media scheme is.

## Security Notes

- Inbound downloads fetch sender-supplied URLs through an egress guard (`policy_checked_download_client`): https-only by default (plain http only when `ALLOW_PRIVATE_IPS=1`), embedded credentials rejected, DNS resolved up front with non-public addresses (private/loopback/link-local/CGNAT/ULA/unspecified/broadcast/multicast) blocked unless `ALLOW_PRIVATE_IPS=1` opts out, the client pinned to the validated addresses (anti-rebind), and redirects plus environment proxies disabled. Both download paths (`download_oob_file`, `download_aesgcm_file`) use it.
- Size limits are enforced during streaming, and the per-stanza URL count and download concurrency are bounded, so a crafted stanza cannot exhaust memory or stall the connection.
- The local bridge HTTP surface rejects non-loopback clients. When `XMPP_BRIDGE_TOKEN` is configured, it also requires Bearer authentication before configure, poll, and send operations. Both checks run as middleware (`authorize_middleware`) **before** any request body is buffered, so unauthenticated or non-loopback requests are rejected with 401/403 without the `Json` extractor reading the body. `/v1/messages/send` bodies are bounded at 70 MiB (`MAX_SEND_BODY_BYTES`) — sized to the host's 50 MB outbound attachment cap plus base64 inflation — so a photo-sized send is never rejected by axum's 2 MiB default body limit while a runaway client still cannot exhaust bridge memory. The raised limit is scoped to the send route only; every other bridge route keeps the 2 MiB default, closing the pre-auth memory-amplification window from `docs/ops/XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md` (concurrent unauthenticated 70 MiB POSTs to control routes).
- Automatic LunarVision requests accept only `http://localhost` or loopback-IP base URLs, disable redirects and environment proxies, inject configured `VISION_AUTH_TOKEN` from `SecretString`, and cap both sidecar response and model-visible extraction sizes. An incompatible URL disables this automatic path without aborting daemon startup. The accepted message's text is never forwarded to the sidecar; a fixed prompt drives analysis. Enrichment runs inside normal message timeout supervision after inbound hooks.
- Explicit `vision-analyze` WASM calls receive that token only inside the host HTTP boundary, scoped to the configured sidecar origin and `/v1/` path; the credential is not serialized into WASM context or sent through an environment proxy.
- LunarVision failures are fail-open only for enrichment: original image bytes remain on the existing multimodal path, while credentials and source URLs remain excluded from model-visible text.

## Implementation Status

Implemented and unit-tested (`cargo test channels::xmpp`); the standalone `xmpp-bridge` builds in release. Delivered in six phases:

1. **Capability advertisement** — `disco#info` responder, ping, `service-unavailable` for other IQs, XEP-0115 caps in presence.
2. **Download hardening** — bounded concurrency, per-stanza URL cap, streaming size enforcement.
3. **Encrypted media** — `aesgcm://` (XEP-0454) download + AES-256-GCM decrypt; decrypted-body URL re-parsing.
4. **Filename robustness** — `filename_from_url` (no longer drops extensionless/opaque names).
5. **Automatic LunarVision enrichment** — bounded OCR/VL preprocessing with optional host-side Bearer auth, shared by legacy and Engine V2 while preserving original multimodal image input.
6. **Outbound body limit fix** (work item #42) — `/v1/messages/send` gained an explicit 70 MiB `DefaultBodyLimit`; photo-sized base64 attachments previously exceeded axum's 2 MiB default and failed with HTTP 413 before the send handler ran, so agents could not attach and send ordinary images. The limit is layered on the send route only (other routes keep the 2 MiB default) and authorization was moved into pre-body middleware (`docs/ops/XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md` follow-up).

**Remaining:** live end-to-end validation against a real server (e.g. Conversations/Gajim → agent over a working XEP-0363 host). This remains file-transfer hardening work, not a gap in LunarVision routing.
