# XMPP File-Transfer Security and Robustness Review R

> **Historical point-in-time review.** The attacker-directed URL and credential
> URL handling described as open below was hardened after commit `f4717e3`:
> current inbound downloads enforce HTTPS/public-address/DNS-pinning policy,
> disable redirects and environment proxies, and strip/redact recognized URLs.
> Other findings remain review evidence unless separately closed. Line references
> and the verdict are not a current-source status report; see
> [`XMPP_TRANSFERS.md`](../XMPP_TRANSFERS.md) and
> [`XMPP_KNOWN_ISSUES.md`](../XMPP_KNOWN_ISSUES.md).

- Review date: 2026-07-28
- Reviewed at commit: `f4717e3`

## Scope

This was an independent, code-first review of the remaining polish and hardening work in LunarWing's XMPP file-transfer pipeline. It covered:

- Commits `5656ddf`, `8979d77`, `4afd7ba`, `f3fe8d5`, and `13f5d80`.
- The native XMPP channel, including HTTP Upload discovery and XEP-0363 slot use, inbound XEP-0066 OOB handling, `aesgcm://` downloads, attachment limits, reconnect behavior, and OMEMO integration:
  - `ic/src/channels/xmpp/mod.rs`
  - `ic/src/channels/xmpp/config.rs`
  - `ic/src/channels/xmpp/omemo/`
- Shared attachment types and downstream consumers:
  - `ic/src/channels/channel.rs`
  - `ic/src/channels/mod.rs`
  - `ic/src/channels/wasm/host.rs`
  - `ic/src/agent/attachments.rs`
  - `ic/src/agent/agent_loop.rs`
  - `ic/src/vision_enrichment.rs`
- The active bridge and WASM channel path used to carry XMPP attachments into LunarWing:
  - `ic/bridges/xmpp-bridge/src/main.rs`
  - `ic/openclaw-ports/xmpp/bridge/src/lib.rs`
  - `ic/channels-src/xmpp/src/lib.rs`
  - `ic/channels-src/xmpp/xmpp.capabilities.json`
- Current configuration defaults in `ic/src/config/channels.rs`.

The top-level `xmpp_bridge/` stub was not treated as an implementation. The background architecture and operations documents were used only for orientation; every conclusion below was verified against the current code.

## Verdict summary

The implementation has several good local controls: sender admission precedes attachment retrieval, individual downloads are streamed through a hard cap, download concurrency and time are bounded, AES-GCM authentication is checked, and the LunarVision sidecar path is narrowly constrained. Those controls materially reduce accidental abuse.

The remaining security boundary is nevertheless incomplete. An admitted XMPP peer can still make LunarWing fetch attacker-selected URLs without a destination or redirect policy. An OMEMO stanza can also carry unauthenticated cleartext OOB extensions that are processed after successful decryption. Outbound "encrypted" transfers upload the original file bytes and encrypt only the URL-bearing message. Finally, the native downloader, bridge queue, JSON/base64 transport, and WASM host enforce incompatible aggregate limits; a single oversized message can consume substantial memory and wedge polling before the host's 20 MiB boundary is reached.

## Findings

### High — Inbound OOB retrieval is an SSRF and insecure-transport primitive

References:

- `ic/src/channels/xmpp/mod.rs:2866-2924`
- `ic/src/channels/xmpp/mod.rs:3018-3059`
- `ic/src/channels/xmpp/mod.rs:3083-3093`
- `ic/src/channels/xmpp/mod.rs:3139-3161`

The inbound path constructs a general-purpose `reqwest::Client` with a timeout, then follows peer-controlled OOB URLs. It does not restrict schemes, reject credentials, constrain redirects, bypass environment proxies, resolve and validate destination addresses, or block loopback, link-local, private, carrier-grade NAT, unique-local, and cloud metadata ranges. Clear OOB URLs may use plain HTTP. The `aesgcm://` parser forces an `https://` fetch but leaves the host and destination unrestricted, so encryption of the payload does not remove the SSRF capability.

Sender and room admission narrow who can trigger this, but admitted, paired, wildcard-allowed, or compromised peers remain untrusted. Redirects and DNS changes also mean validating only the textual initial host would be insufficient.

Suggested hardening:

- Route all inbound attachment URLs through one URL-policy component.
- Require HTTPS outside an explicit local-development mode; reject userinfo and malformed or excessively long URLs.
- Resolve every destination and reject non-public address classes before connecting; validate the actual connected address to resist DNS rebinding.
- Disable redirects or revalidate every hop with a small hop limit.
- Disable ambient proxy use for attachment fetches unless a separately configured, policy-enforcing egress proxy is required.
- Consider a domain allowlist and isolated egress service for high-assurance deployments.

### High — OMEMO decryption does not authenticate the OOB attachment source

References:

- `ic/src/channels/xmpp/mod.rs:1386-1434`
- `ic/src/channels/xmpp/mod.rs:1538-1546`
- `ic/src/channels/xmpp/mod.rs:2866-2886`
- `ic/src/channels/xmpp/mod.rs:1054-1066`

After successfully decrypting an OMEMO body, the code still extracts clear XEP-0066 OOB extensions from the outer stanza payloads. Those extensions are not inside the authenticated OMEMO ciphertext. An XMPP server, room component, or other stanza-mutating intermediary can therefore add an OOB URL to an otherwise authentic encrypted message and make LunarWing fetch and ingest content not selected by the sender.

The outbound code already recognizes the confidentiality problem and omits clear OOB payloads for encrypted stanzas, but the inbound side does not enforce the corresponding provenance rule.

Suggested hardening:

- Track whether the accepted body came from successful OMEMO decryption.
- For encrypted messages, ignore all outer cleartext OOB extensions.
- Accept encrypted-message attachments only from a manifest or `aesgcm://` reference inside the authenticated plaintext.
- Longer term, cryptographically bind filename, media type, byte length, and content digest to that manifest.

### High — Outbound OMEMO transfers upload the original file bytes

References:

- `ic/src/channels/xmpp/mod.rs:2638-2670`
- `ic/src/channels/xmpp/mod.rs:2674-2724`
- `ic/src/channels/xmpp/mod.rs:997-1059`
- `ic/src/channels/xmpp/mod.rs:1803-1813`
- `ic/src/channels/xmpp/mod.rs:1907-1977`

`http_put_file` sends `attachment.data` directly to the upload slot. OMEMO encryption happens later, when the text containing the returned GET URL is built. Thus the XMPP upload service receives the plaintext file, and anyone with access to the upload object can read it. The PUT and GET slot URLs also receive no scheme, redirect, or destination validation.

This is encrypted URL delivery, not end-to-end encrypted file transfer.

Suggested hardening:

- Implement an XEP-0454-style flow: encrypt the file locally with a fresh random AES-GCM key and IV, upload only ciphertext, and place the key-bearing `aesgcm://` reference solely inside the OMEMO-authenticated body.
- Bind the expected ciphertext size and content digest to the encrypted metadata.
- Require HTTPS for upload slot PUT and GET URLs, apply destination policy, and reject unsafe cross-origin redirects.
- Keep product and operator wording explicit until the file bytes themselves are end-to-end encrypted.

### High — Aggregate limits diverge across the native channel, bridge, and WASM host

References:

- `ic/src/channels/xmpp/mod.rs:56-63`
- `ic/src/channels/xmpp/mod.rs:2893-2924`
- `ic/bridges/xmpp-bridge/src/main.rs:25-26`
- `ic/bridges/xmpp-bridge/src/main.rs:414-434`
- `ic/bridges/xmpp-bridge/src/main.rs:489-518`
- `ic/channels-src/xmpp/src/lib.rs:152-212`
- `ic/channels-src/xmpp/src/lib.rs:342-395`
- `ic/channels-src/xmpp/xmpp.capabilities.json:79-108`
- `ic/src/channels/wasm/host.rs:44-65`
- `ic/src/channels/wasm/host.rs:285-349`

The native channel permits ten successful attachments of up to 20 MiB each and collects all results into memory. The bridge then base64-encodes and retains whole attachments in a queue bounded only by 2,048 message entries. Polling clones all messages newer than the cursor into one response. The WASM manifest caps an HTTP response at 30 MiB, while the host's intended decoded aggregate attachment limit is 20 MiB and is enforced only after the bridge response has been received and decoded.

A message containing multiple individually valid files can therefore:

- Hold up to roughly 200 MiB of raw attachment data in the native layer.
- Expand by about one third when base64-encoded and be retained in the bridge queue.
- Exceed the bridge-to-WASM HTTP response cap before host validation.
- Cause polling to retry the same oversized response because the cursor is advanced only after successful processing.

The bridge router also declares no explicit request-body limit aligned with the channel's advertised 30 MiB request allowance (`ic/bridges/xmpp-bridge/src/main.rs:108-114`), making the outbound contract dependent on framework defaults.

Suggested hardening:

- Define one end-to-end decoded aggregate budget, preferably no greater than the host's existing 20 MiB limit, and enforce it before and during download.
- Use a byte-weighted semaphore in addition to count concurrency.
- Stream or spool attachments rather than retaining simultaneous raw and base64 copies.
- Bound the bridge queue by bytes, add expiration/acknowledgement, and page polls by both message count and encoded bytes.
- Reject oversized bridge requests before base64 decoding and set an explicit request-body limit consistent with the decoded budget.
- Advance or quarantine a poison message through a structured dead-letter path instead of retrying it forever.

### High — Signed URLs and `aesgcm://` keys can leak through errors, logs, and failed message content

References:

- `ic/src/channels/xmpp/mod.rs:2907-2917`
- `ic/src/channels/xmpp/mod.rs:2949-2959`
- `ic/src/channels/xmpp/mod.rs:3026-3030`
- `ic/src/channels/xmpp/mod.rs:3087-3093`
- `ic/src/channels/xmpp/mod.rs:3124-3128`
- `ic/src/channels/xmpp/mod.rs:1538-1546`

The per-download warning logs the complete URL. Transport errors are propagated with `reqwest` context that may also include the URL. Successful attachments retain the full source URL, including signed query strings; encrypted attachments retain the fragment containing the decryption key. Finally, URL stripping is based only on successfully downloaded attachments, so a failed `aesgcm://...#key` token remains in normal message content and is eligible to enter the agent, model, and persistence paths.

Suggested hardening:

- Introduce a single safe URL formatter that removes userinfo, query strings, and fragments from logs and user-facing errors.
- Never put an AES key-bearing URL in a general `source_url` field; retain only a redacted origin or opaque attachment identifier.
- Strip recognized sensitive attachment tokens from message content regardless of fetch success, replacing failures with a safe attachment-error marker.
- Ensure error types carry classified fields rather than raw URL strings, and add regression tests for tracing and serialized error output.

### Medium — HTTP transfers block the single XMPP client event loop

References:

- `ic/src/channels/xmpp/mod.rs:919-934`
- `ic/src/channels/xmpp/mod.rs:959-1075`
- `ic/src/channels/xmpp/mod.rs:1271-1308`
- `ic/src/channels/xmpp/mod.rs:1538-1546`
- `ic/src/channels/xmpp/mod.rs:2423-2425`

Inbound stanza processing awaits the complete attachment batch inside the same `tokio::select!` loop that services XMPP events. Outbound attachments are uploaded serially in that loop, with a 120-second slot/upload timeout per file. The loop cannot promptly process heartbeats, disconnects, IQs, later messages, or shutdown while those operations are pending. Ten inbound files can also consume multiple 30-second concurrency waves.

Suggested hardening:

- Move HTTP transfer work to a supervised transfer manager with global count and byte limits.
- Return completion events to the XMPP loop over a bounded channel.
- Attach cancellation tokens to connection generation and shutdown so stale work can be aborted or explicitly completed after reconnect.
- Keep IQ interactions in the client loop, but do not await bulk HTTP I/O there.

### Medium — Outbound admission is message-bounded, not attachment- or byte-bounded

References:

- `ic/src/channels/xmpp/mod.rs:207-207`
- `ic/src/channels/xmpp/mod.rs:241-285`
- `ic/src/channels/xmpp/mod.rs:959-997`
- `ic/bridges/xmpp-bridge/src/main.rs:437-486`

The native queue is bounded to 64 messages, but the enqueue path does not validate attachment count, individual size, aggregate bytes, filename, or media type. The bridge decodes all submitted base64 attachments before invoking that path. A single queued message can therefore carry an unbounded in-memory payload from an authenticated local caller. The rate limiter charges once at enqueue even though the event loop may emit a text stanza plus one stanza per successful attachment.

Suggested hardening:

- Validate count, per-file size, aggregate size, media type, and filename before allocation and queueing.
- Make the queue byte-bounded and reserve capacity before decoding or copying payloads.
- Charge rate limits for the actual stanza fan-out, or deliver all URLs in one stanza where interoperability allows it.
- Use a streaming or file-backed submission API for larger payloads.

### Medium — Send completion, partial failure, retry, and progress are not represented

References:

- `ic/src/channels/xmpp/mod.rs:273-285`
- `ic/src/channels/xmpp/mod.rs:970-993`
- `ic/src/channels/xmpp/mod.rs:1069-1075`
- `ic/bridges/xmpp-bridge/src/main.rs:478-486`

The public native and bridge APIs report success after queueing, not after slot discovery, upload, or XMPP send. Upload failures are logged and skipped; later files may still be sent. Send failures are logged without retry or durable status. An attachment-only response in which all uploads fail may yield no recipient-visible message at all. There is no progress, cancellation, delivery receipt, or structured partial-success result.

Suggested hardening:

- Return a transfer/job identifier and expose states such as queued, discovering, uploading, sent, failed, and partially sent.
- Add a completion channel for in-process callers and a status endpoint for bridge callers.
- Retry only well-defined idempotent phases with bounded exponential backoff and jitter; obtain a new slot when reuse is unsafe.
- Aggregate per-file errors into one recipient- and operator-visible result without exposing secret URLs.
- Support cancellation and optional delivery receipts.

### Medium — Plaintext fallback is enabled by default and applies to attachment URLs

References:

- `ic/src/config/channels.rs:368-425`
- `ic/src/channels/xmpp/mod.rs:738-756`
- `ic/src/channels/xmpp/mod.rs:1793-1796`
- `ic/src/channels/xmpp/mod.rs:1054-1066`

`allow_plaintext_fallback` defaults to true. If no usable OMEMO recipients are available, the body containing uploaded-file URLs may be sent as plaintext and accompanied by a clear OOB payload. Operators may reasonably interpret OMEMO-enabled file transfer as fail-closed even if they accept plaintext fallback for ordinary text.

Suggested hardening:

- Split text and attachment downgrade policy.
- Default `allow_plaintext_file_fallback` to false.
- Require an explicit per-transfer or operator decision before disclosing an attachment URL in plaintext.
- Surface the downgrade in transfer status and audit logs without logging the URL.

### Medium — Loopback bridge authentication is optional

References:

- `ic/bridges/xmpp-bridge/src/main.rs:88-100`
- `ic/bridges/xmpp-bridge/src/main.rs:414-486`
- `ic/bridges/xmpp-bridge/src/main.rs:521-545`

The bridge rejects non-loopback clients, but when no token is configured, every local process can poll received messages and attachment bytes, submit outbound attachments, and use other bridge routes. On shared or multi-tenant hosts, loopback is not an authorization boundary. When a token is configured, comparison is a normal string equality check.

Suggested hardening:

- Require a high-entropy secret for TCP operation and fail startup when it is absent outside an explicit development mode.
- Prefer a mode-`0600` Unix socket with peer-credential authorization for same-host deployment.
- Store the token in a secret-bearing type and use constant-time comparison.
- Apply route-specific authorization and audit failed requests without echoing credentials.

### Medium — OMEMO private-state files rely on ambient filesystem permissions

References:

- `ic/src/channels/xmpp/omemo/store.rs:152-170`
- `ic/src/channels/xmpp/omemo/store.rs:462-495`
- `ic/src/channels/xmpp/omemo/store.rs:518-527`
- `ic/src/channels/xmpp/omemo/store.rs:829-838`

The OMEMO store serializes private identity and session state to disk. Directories are created without an explicit `0700` mode, files without an explicit `0600` mode, and atomic writes use a predictable sibling `.tmp` path opened with `File::create`, which follows an existing symlink. Confidentiality and resistance to local link attacks therefore depend on the process umask and exclusive control of the parent directory.

Remote JIDs are encoded before becoming path components, so this is not a remote path-traversal issue; it is local secret-store hardening.

Suggested hardening:

- Create and verify private directories as `0700` and state files as `0600`.
- Use a unique same-directory temporary file opened with `create_new` and no-follow semantics.
- Verify that the parent and target are not symlinks and are owned by the expected account.
- Flush the file and parent directory around rename where crash durability matters.

### Low — Media type and filename metadata are trusted too early

References:

- `ic/src/channels/xmpp/mod.rs:3036-3077`
- `ic/src/channels/xmpp/mod.rs:3112-3127`
- `ic/src/channels/channel.rs:15-35`
- `ic/src/vision_enrichment.rs:84-106`

Plain OOB attachments derive `AttachmentKind` from the server's `Content-Type`; encrypted attachments infer MIME from the URL filename extension. `AttachmentKind::Image` is then sufficient to select automatic LunarVision enrichment. A peer can label arbitrary bytes as an image or use misleading filename metadata.

Suggested hardening:

- Sniff a conservative set of supported formats after download and reconcile detected type, declared type, and extension.
- Reject active or ambiguous formats unless explicitly supported.
- Normalize and length-limit display filenames, remove control and bidi characters, and keep original metadata separate from safe display names.
- Make downstream analyzers treat all media bytes and metadata as untrusted even after classification.

### Low — URL collection has duplicate and tokenization edge cases

References:

- `ic/src/channels/xmpp/mod.rs:2879-2886`
- `ic/src/channels/xmpp/mod.rs:2928-2946`
- `ic/src/channels/xmpp/mod.rs:2983-2990`
- `ic/channels-src/xmpp/src/lib.rs:366-390`

The initial duplicate set is built from OOB URLs but is not updated as URLs are appended from the body. Duplicate OOB payloads are also preserved. `aesgcm://` discovery is whitespace-based, so ordinary trailing punctuation makes a token fail parsing. In the WASM channel, stored attachment IDs are derived from `oob-{filename}`, so two same-named attachments in one message can collide and reference overwritten data.

Suggested hardening:

- Insert into one mutable `seen` set for every accepted source.
- Parse body references with a bounded URL-aware tokenizer and normalize only the safe portions needed for comparison.
- Generate attachment IDs from message identity plus stable index or content digest, never from filename alone.
- Add tests for duplicate OOB elements, duplicate body URLs, punctuation, same-name files, and mixed successful/failed downloads.

### Low — Upload-service caching and discovery do not follow connection lifecycle

References:

- `ic/src/channels/xmpp/mod.rs:937-945`
- `ic/src/channels/xmpp/mod.rs:2477-2588`

The cached HTTP Upload service is returned immediately and is not cleared when the XMPP client disconnects and is rebuilt. A server topology or capability change can leave the process using a stale service until restart. Discovery also walks returned items serially without an explicit item-count budget.

Suggested hardening:

- Scope the cache to the connection generation and clear it on disconnect.
- On slot failure, invalidate and rediscover once before returning an error.
- Cap discovery items, apply one overall deadline, and prefer services advertising an acceptable maximum size.

### Low — Transfer lifecycle is detached from channel health and shutdown

References:

- `ic/src/channels/xmpp/mod.rs:828-862`
- `ic/src/channels/xmpp/mod.rs:1246-1248`
- `ic/src/channels/channel.rs:479-482`

The XMPP worker is spawned without retaining a join handle. `health_check` always succeeds, and XMPP does not override the trait's no-op shutdown method. There is no explicit way to report a dead worker, cancel in-flight HTTP operations, drain or fail queued transfers, or wait for the XMPP stream to close.

Suggested hardening:

- Retain the worker handle, connection state, last error, queue depth, and in-flight counts.
- Add a cancellation token and bounded graceful shutdown.
- Have health reflect worker liveness and connection readiness, with separately reported degraded transfer-service state.
- Resolve every queued transfer as completed, cancelled, or failed during shutdown.

### Nit — Native `Channel` response methods silently discard attachment paths

References:

- `ic/src/channels/channel.rs:245-275`
- `ic/src/channels/xmpp/mod.rs:1094-1115`
- `ic/src/channels/xmpp/mod.rs:1220-1243`

`OutgoingResponse` includes attachment paths, but the native XMPP `respond` and `broadcast` implementations forward only text. The bridge has a custom attachment submission path, so behavior depends on which integration surface is used.

Suggested hardening:

- Wire the standard `OutgoingResponse.attachments` contract into the XMPP upload path, or return an explicit unsupported-attachment error.
- Add a contract test so attachments cannot be silently dropped.

## Already solid

- **Peer admission happens before network retrieval.** Direct-message policy, pairing, sender allowlists, and room allowlists are checked before `extract_inbound_attachments` is invoked (`ic/src/channels/xmpp/mod.rs:1330-1374`, `ic/src/channels/xmpp/mod.rs:1538-1546`). The default resolved configuration allows only the account's own bare JID and configures no rooms (`ic/src/config/channels.rs:381-401`).
- **The XMPP client is configured for STARTTLS.** The current dependency enables tokio-xmpp's STARTTLS path, and channel construction uses that client (`ic/Cargo.toml:184-187`, `ic/src/channels/xmpp/mod.rs:828-840`).
- **Individual inbound files are streamed through a hard cap.** The collector stops after the configured maximum instead of trusting `Content-Length`, while the header is also used for an early rejection (`ic/src/channels/xmpp/mod.rs:2992-3011`, `ic/src/channels/xmpp/mod.rs:3043-3059`). Downloads have a 30-second client timeout and are limited to four concurrent futures per message (`ic/src/channels/xmpp/mod.rs:56-63`, `ic/src/channels/xmpp/mod.rs:2893-2924`).
- **Malformed OOB elements fail closed.** Invalid OOB payloads are ignored rather than panicking (`ic/src/channels/xmpp/mod.rs:2928-2934`).
- **AES-GCM attachments are authenticated.** Parsing enforces expected key and IV lengths, and decryption rejects a bad tag (`ic/src/channels/xmpp/mod.rs:3139-3194`).
- **Outbound encrypted stanzas avoid a clear OOB duplicate.** The sender omits the OOB extension when the body is encrypted (`ic/src/channels/xmpp/mod.rs:1054-1066`).
- **LunarVision enrichment has strong sidecar controls.** It accepts only loopback HTTP endpoints with no credentials, query, or fragment; disables proxies and redirects; applies connect/request/response limits; caps each image at 10 MiB; and analyzes at most two concurrently (`ic/src/vision_enrichment.rs:13-18`, `ic/src/vision_enrichment.rs:62-75`, `ic/src/vision_enrichment.rs:84-127`, `ic/src/vision_enrichment.rs:150-222`).
- **Attachment URLs are excluded from normal model attachment text.** Downstream formatting escapes attacker-controlled metadata and sends filename, MIME, extracted text, and image bytes as appropriate without copying `source_url` into the model-facing attachment description (`ic/src/agent/attachments.rs:30-42`, `ic/src/agent/attachments.rs:85-170`).
- **Current save paths resist remote traversal.** OOB filenames come from one final URL segment (`ic/src/channels/xmpp/mod.rs:2983-2990`); extracted document names remove separators, NULs, and leading dots before workspace writes (`ic/src/agent/agent_loop.rs:1360-1392`); OMEMO remote identifiers are hex-encoded before use as path components (`ic/src/channels/xmpp/omemo/store.rs:566-590`, `ic/src/channels/xmpp/omemo/store.rs:841-845`).
- **The WASM host has a useful final validation boundary.** It enforces attachment count, allowed MIME prefixes, a 20 MiB aggregate, and a 20 MiB per-stored-attachment cap (`ic/src/channels/wasm/host.rs:44-65`, `ic/src/channels/wasm/host.rs:285-349`, `ic/src/channels/wasm/host.rs:366-410`). The limitation is that these checks currently occur after the higher-amplification native and bridge stages.

## Recommended next steps

1. **Close the trust-boundary issues first.** Add a shared URL/egress policy, reject clear OOB data on OMEMO-decrypted messages, and redact or remove all key- and token-bearing URLs from failures, logs, and message content.
2. **Make encrypted file transfer actually end to end.** Encrypt bytes before HTTP upload and authenticate a compact attachment manifest inside OMEMO.
3. **Adopt one end-to-end resource contract.** Enforce a decoded aggregate byte budget before downloading or decoding, then align native, bridge, HTTP, WASM, and downstream limits. Bound queues by bytes and page bridge polls.
4. **Decouple transfers from the XMPP event loop.** Add supervised, cancellable transfer jobs with count and byte semaphores, completion state, and reconnect/shutdown semantics.
5. **Fail closed on confidentiality.** Disable plaintext fallback for attachments by default and require mandatory bridge authentication outside explicit development mode.
6. **Improve operator and user feedback.** Add job IDs, progress/state reporting, bounded retries, cancellation, partial-failure summaries, and meaningful health signals.
7. **Add focused adversarial tests.** Cover private and redirected destinations, DNS rebinding policy, clear OOB injection into OMEMO stanzas, URL redaction, aggregate-limit enforcement across the bridge, poll poison-message recovery, duplicate/same-name attachments, reconnect cancellation, and shutdown.

## Review summary

The review traced the requested commits and the current transfer path from XMPP stanza admission through OOB/`aesgcm://` download, HTTP Upload, OMEMO wrapping, bridge queueing, WASM decoding, agent attachment consumption, and automatic vision enrichment. The most important remaining work is to prevent attacker-directed network access, authenticate attachment provenance for encrypted messages, encrypt outbound file bytes rather than only their URL, and replace incompatible per-layer limits with one byte-bounded end-to-end transfer contract.
