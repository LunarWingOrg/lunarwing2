# XMPP File Transfer and LunarVision Review (A)

Review date: 2026-07-28

Review baseline: `dcb5dfc`, including the automatic XMPP image enrichment work
from `5656ddf`.

## Scope

This review covered the current inbound and outbound XMPP attachment paths, the
bridge and WASM attachment transport, automatic LunarVision enrichment, scoped
vision-tool authentication, the OCR sidecar request path, and the focused tests
and documentation delivered with the integration.

The automatic enrichment middleware itself is a solid foundation: it restricts
the configured sidecar to loopback HTTP, disables redirects and environment
proxies, bounds image and response sizes, limits concurrent analyses, keeps the
user's text out of the sidecar request, uses a host-side `SecretString`, escapes
model-visible extraction, and preserves the original image for multimodal
fallback. The migration code also correctly regenerates the host-local vision
credential instead of importing a stale sidecar token.

The integration is not ready to be called fully hardened. The findings below
include three release-blocking security or liveness issues, followed by parity
and correctness work.

## P0 Findings

### 1. Failed encrypted downloads expose the XEP-0454 key URL

`extract_inbound_attachments()` returns only successful downloads, and
`strip_attachment_urls()` removes only the `source_url` values attached to those
successes (`ic/src/channels/xmpp/mod.rs:2866-2923` and
`ic/src/channels/xmpp/mod.rs:2949-2961`). If an encrypted download times out, is
oversized, has an invalid GCM tag, or otherwise fails, its original
`aesgcm://...#IV+key` token remains in `IncomingMessage.content` at
`ic/src/channels/xmpp/mod.rs:1538-1546`. It can then enter model context and
persistent conversation history.

The same failure path logs the complete URL at
`ic/src/channels/xmpp/mod.rs:2911-2915`. For XEP-0454, the fragment is not benign
metadata: it contains the IV and 32-byte media decryption key. Ordinary signed
HTTP upload URLs can also carry credentials in their query strings, so logging
the full URL is unsafe for both encrypted and unencrypted transfers.

Required change: track recognized attachment tokens separately from successful
attachments, remove or replace every recognized `aesgcm://` token before the
message is emitted, and log only a redacted scheme/host/path or a stable digest.
Regression tests should cover timeout, size rejection, invalid key material, and
GCM authentication failure, and assert that neither captured logs nor emitted
content contains the fragment.

### 2. Sender-controlled downloads still have unrestricted SSRF and redirects

The OOB client is built with reqwest defaults at
`ic/src/channels/xmpp/mod.rs:2893-2896`. Both `download_oob_file()` and
`download_aesgcm_file()` fetch sender-controlled targets without scheme, host,
resolved-address, proxy, or redirect enforcement
(`ic/src/channels/xmpp/mod.rs:3018-3109`). An allowed DM sender or any participant
in an allowed room can therefore probe loopback, private networks, link-local
services, and cloud metadata endpoints. Redirects can move an initially public
URL to an internal target.

This risk is already acknowledged in `docs/ops/XMPP_TRANSFERS.md` and
`docs/architecture/XMPP_FILE_TRANSFERS.md`; deferring it leaves a direct network
boundary violation in an automatically invoked path. The generic
`ALLOW_PRIVATE_IPS=1` harness setting should not silently authorize
sender-selected attachment destinations.

Required change: introduce a dedicated attachment-download policy that permits
only HTTP(S), disables redirects unless every hop is revalidated, resolves and
rejects non-public addresses immediately before connecting, and prevents DNS
rebinding by connecting to the validated address. Disable environment proxies
unless an explicitly configured download proxy is the enforcement boundary.
Add loopback, RFC1918, link-local, IPv6, redirect, mixed-DNS, and rebinding tests.

### 3. Attachment limits do not compose across the bridge and can poison polling

The native XMPP path permits ten 20 MiB files, or 200 MiB of raw attachment data,
per stanza (`ic/src/channels/xmpp/mod.rs:57-63`). The bridge then base64-encodes
all retained bytes into one JSON response
(`ic/bridges/xmpp-bridge/src/main.rs:489-515`). In contrast, the XMPP WASM
capability permits only a 30 MiB HTTP response
(`ic/channels-src/xmpp/xmpp.capabilities.json:107-108`), while the channel host
enforces lower aggregate storage/emission limits
(`ic/src/channels/wasm/host.rs:44-48` and
`ic/src/channels/wasm/host.rs:370-403`).

`messages_handler()` returns every queued message newer than the cursor in one
response (`ic/bridges/xmpp-bridge/src/main.rs:414-434`). Two near-limit files in
one message already exceed the 30 MiB response cap after base64 expansion. The
WASM poll fails before advancing its cursor, and later polls request the same
oversized response plus any new messages. This can wedge XMPP delivery while the
bridge also retains a large allocation.

Required change: define one end-to-end aggregate byte budget before downloads
begin, no larger than the smallest downstream budget after encoding overhead.
The bridge poll API must paginate by both message count and serialized bytes and
must not allow one item to make the cursor permanently unadvanceable. Prefer an
opaque attachment/blob handle or bounded binary retrieval over embedding all
bytes in the messages JSON response. Add a boundary test that exercises native
download, bridge serialization, WASM poll, and host emission together.

## P1 Findings

### 4. Legacy-agent safety checks do not inspect LunarVision output

Automatic enrichment runs before engine selection
(`ic/src/agent/agent_loop.rs:1482-1497`). Engine V2 augments attachment text
before input validation, policy checks, and secret scanning
(`ic/src/bridge/router.rs:2726-2760`). The legacy path instead validates only the
original user text at `ic/src/agent/thread_ops.rs:325-367` and appends OCR/VL
content later at `ic/src/agent/thread_ops.rs:440-472`.

XML escaping and the phrase "untrusted attachment content" prevent delimiter
breakout but do not neutralize natural-language prompt injection in an image or
a compromised VL response. The two engines therefore have different safety
properties for the same XMPP attachment.

Required change: after pending-auth and command handling are separated from
ordinary input, run legacy safety validation, policy checks, and secret scanning
against the same augmented text sent to the model. Add a legacy regression with
malicious OCR text and keep an Engine V2 parity case.

### 5. Outbound OMEMO attachments are not end-to-end encrypted media

Outbound attachment bytes are uploaded unchanged in `http_put_file()`
(`ic/src/channels/xmpp/mod.rs:2637-2669`). The returned GET URL is placed in an
OMEMO-encrypted message body when applicable
(`ic/src/channels/xmpp/mod.rs:963-1059`), but the uploaded object itself is not
encrypted with a fresh AES-GCM key and no outbound `aesgcm://` URL is produced.
The upload service can therefore read attachments sent in an encrypted DM or
room.

This makes the broad documentation claim that file transfers work "including
OMEMO-encrypted" misleading for the outbound direction. Either implement
outbound XEP-0454 before upload, or explicitly document that only the URL stanza
is OMEMO-protected and that the upload operator can read the file. Encryption-
required rooms should not imply media E2EE until the former is implemented.

Slot URLs also need HTTPS and redirect validation before file bytes or slot
headers are sent. A server-provided PUT redirect must not relay an attachment or
custom upload credential to an arbitrary origin.

### 6. The sidecar admits expensive work before authentication and lacks a work cap

All OCR/vision routes collect the complete request body, then consume the global
rate limiter, and only then authenticate
(`projects/ocr-sidecar/src/main.rs:1869-1914`). An unauthenticated peer can
allocate maximum-sized bodies and exhaust the shared rate budget. This matters
on multi-tenant hosts because every local user can generally reach loopback
published ports even though the port is not externally bound.

After admission, Tesseract and PaddleOCR subprocesses have neither a semaphore
nor an execution timeout (`projects/ocr-sidecar/src/main.rs:649-717`). The
client's 45-second timeout does not cancel a sidecar subprocess, and the 10/s
arrival limiter does not bound concurrent long-running children.

Required change: authenticate headers before rate limiting and before reading a
body, reject unauthorized requests without consuming tenant capacity, and add a
small global OCR/VL work semaphore plus process timeouts that kill and reap child
processes. Keep the decoded-size check as a second boundary after the encoded
body limit.

### 7. Encrypted images with opaque upload paths bypass image handling

After AES-GCM decryption, MIME type is inferred only from the URL filename
(`ic/src/channels/xmpp/mod.rs:3112-3125`). XEP-0363 services commonly use opaque
or extensionless object paths. Such an image becomes `application/octet-stream`
and `AttachmentKind::Document`, so `VisionEnrichmentMiddleware` skips it and the
provider-native image part is not built.

Required change: detect supported image formats from decrypted magic bytes,
then use a trustworthy declared type or filename extension only as fallback.
Apply the same fallback when an ordinary HTTP response reports a generic
content type. Tests should use extensionless PNG, JPEG, WebP, and TIFF URLs.

### 8. Duplicate URLs and filenames can duplicate work or attach the wrong bytes

The body URL loop checks only the initial OOB set and never updates its `known`
set, so repeated `aesgcm://` tokens in one body can be downloaded repeatedly
(`ic/src/channels/xmpp/mod.rs:2877-2887`). The WASM channel then derives storage
IDs solely as `oob-{filename}` (`ic/channels-src/xmpp/src/lib.rs:342-395`). Two
different URLs with the same filename overwrite the same host data entry; the
first emitted attachment receives the last stored bytes and the later one is
empty.

Required change: deduplicate normalized candidate URLs while preserving order,
and carry the bridge `message_id` plus an attachment index or a generated stable
attachment ID through the contract. Filenames are display metadata, not
identifiers.

## P2 Findings

### 9. VL backend responses and error text are unbounded

`run_vl()` accepts default proxy and redirect behavior, parses a successful body
without a response-size cap, and reads an error body without a cap before
including it in a warning (`projects/ocr-sidecar/src/main.rs:953-1035` and
`projects/ocr-sidecar/src/main.rs:1114-1120`). A failed or compromised configured
backend can consume memory or place arbitrary image-derived text and reflected
secrets in logs.

Use a capped streaming reader for both success and error responses, log status
and a bounded redacted diagnostic rather than the complete body, and make proxy
and redirect behavior explicit. Store `VL_API_KEY` in a secret type rather than
an ordinary cloned `String`.

### 10. Cache identity and cache telemetry are inconsistent

The sidecar cache key includes image, prompt, mode, and detail level but omits
`ocr_lang` (`projects/ocr-sidecar/src/main.rs:774-788`). A request in one language
can therefore serve a result to another language. Disk preload also inserts the
encoded cache filename instead of the original hash key
(`projects/ocr-sidecar/src/main.rs:110-132`), forcing later lookups back to disk,
and cache-hit responses return before request/latency counters are updated
(`projects/ocr-sidecar/src/main.rs:1066-1083`).

Include every output-affecting field in an unambiguous serialized cache key,
restore the original key when loading persisted entries, and define whether
request counters include cache hits. Add restart and cross-language tests.

## Test Gaps

The focused middleware and agent tests verify authenticated construction,
failure fallback, output bounds, XML escaping, persistence behavior, and both
agent engines. They bypass the actual XMPP download, bridge JSON transport, WASM
decode/storage limits, and real sidecar process admission. Current XMPP unit
tests also cover only successful URL stripping, not failed-download secrecy.

Before release, add one contiguous automated path covering:

1. XMPP stanza with OOB and encrypted-media URLs.
2. Success, timeout, redirect, SSRF, oversized, and bad-GCM download outcomes.
3. Bridge polling at aggregate byte boundaries and duplicate filenames.
4. WASM attachment reconstruction and LunarVision enrichment.
5. Legacy and Engine V2 safety/history assertions.

Live Conversations, Gajim, and Dino validation remains necessary because client
URL shapes, MIME behavior, OMEMO body formatting, and upload service responses
are exactly where several of these gaps surface.

## Recommended Order

1. Fix encrypted URL redaction on every path and close inbound SSRF.
2. Establish one aggregate attachment budget and make bridge polling recoverable.
3. Apply attachment safety checks consistently in the legacy engine.
4. Add authenticated sidecar admission and bounded OCR/VL execution.
5. Decide and document outbound XEP-0454 support before claiming OMEMO media E2EE.
6. Fix MIME detection, IDs/deduplication, cache identity, and observability.
7. Run the automated boundary matrix and then the three-client live test.

## Verification Status

Static review was performed against the files and revisions named above. The
focused middleware suite passed with the required six-CPU limit:

```text
taskset -c 0-5 cargo test -j6 vision_enrichment --lib -- --nocapture
test result: ok. 8 passed; 0 failed; 0 ignored; 4193 filtered out

taskset -c 0-5 cargo test -j6 channels::xmpp --lib -- --nocapture
test result: ok. 62 passed; 0 failed; 0 ignored; 4139 filtered out
```

This verifies the existing middleware behavior, not the unresolved bridge,
download-security, sidecar-admission, or live-client gaps listed above.
