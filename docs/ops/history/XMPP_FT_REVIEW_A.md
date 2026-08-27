# XMPP File Transfer and LunarVision Review (A)

> **Historical point-in-time review.** The P0 URL-leak and sender-controlled
> egress findings below were fixed after this review: recognized attachment URLs
> are stripped/redacted on failure paths, and downloads now use the shared HTTPS,
> public-address, DNS-pinning, no-redirect, no-proxy policy. Line references and
> the final verdict describe the reviewed 2026-07-28 branch, not current source.
> See [`XMPP_TRANSFERS.md`](../XMPP_TRANSFERS.md) and
> [`XMPP_KNOWN_ISSUES.md`](../XMPP_KNOWN_ISSUES.md) for current operator status.

Review date: 2026-07-28
Reviewer branch: `aj/kimi-26-20260728-2306`
Reviewed work: merge `031bc52` ("XMPP LunarVision auto-enrichment +
vision-analyze scoped auth", item-16 commit `5656ddf`) and the surrounding XMPP
file-transfer code it builds on. All line references verified against this
branch.

## Scope

- Automatic LunarVision enrichment middleware (`ic/src/vision_enrichment.rs`,
  new, 584 lines) and its wiring (`ic/src/main.rs`, `ic/src/config/mod.rs`,
  `ic/src/agent/agent_loop.rs`, `ic/src/agent/attachments.rs`).
- Host-scoped Bearer auth for the `vision-analyze` WASM tool
  (`ic/src/tools/wasm/wrapper.rs`).
- OCR sidecar auth and robustness changes (`projects/ocr-sidecar/src/main.rs`).
- Kawarimi export/import handling of the vision auth token
  (`ic/scripts/export-tenant.sh`, `ic/scripts/import-tenant.sh`).
- Inbound XMPP attachment download path the enrichment consumes
  (`ic/src/channels/xmpp/mod.rs`).
- Docs: `docs/proposals/XMPP_LUNARVISION_INTEGRATION.md` (claims checked
  against code).

## What is solid (verified, not just claimed)

The enrichment middleware itself is well built and matches its proposal:

- `vision_endpoint()` (`ic/src/vision_enrichment.rs:176-203`) enforces plain
  `http://`, rejects credentials/query/fragment, rejects any path, and accepts
  only `localhost` or loopback IPs. The client (`:69-75`) sets connect and
  request timeouts, `no_proxy()`, and a no-redirect policy.
- Bounds are real: 10 MiB decoded image cap (`:101-109`), 1 MiB response cap
  enforced while streaming, not just via `Content-Length` (`:205-223`),
  100,000-byte extracted-text cap with char-boundary-safe truncation
  (`:297-311`), and at most 2 concurrent analyses (`:114-127`).
- The user’s message text is never sent to the sidecar — the request uses a
  fixed prompt (`:19-20`, `:150-158`).
- Fail-open behavior is correct: any sidecar error leaves the attachment
  unchanged so provider-native multimodal handling still works (`:129-147`).
- `Config.vision_auth_token` is a `SecretString` from `VISION_AUTH_TOKEN`
  (`ic/src/config/mod.rs`), and an invalid `VISION_SERVICE_URL` only disables
  enrichment with a warning — daemon startup continues
  (`ic/src/main.rs:937-956`).
- Enrichment runs inside the supervised message task, after inbound hooks, and
  only for `Submission::UserInput` carrying images
  (`ic/src/agent/agent_loop.rs:1478-1494`) — slow VL inference cannot stall the
  channel receive loop.
- Rendered analysis is labeled "untrusted attachment content" and XML-escaped
  before entering model context, while the raw image part is preserved
  (`ic/src/agent/attachments.rs:128-140`, test at `:279-300`).
- WASM scoped auth (`ic/src/tools/wasm/wrapper.rs:92-151`) re-validates the
  service URL (http, loopback or `host.containers.internal`, no
  credentials/query/fragment/path), injects the token only when origin and
  `/v1/` path prefix match (`:136-151`, `:325-334`), never serializes it into
  WASM context, and bypasses the proxy only for scoped requests (`:464-473`,
  `:557-568`).
- Kawarimi migration is right: export no longer carries `LUNARWING_AUTH_TOKEN`
  (`ic/scripts/export-tenant.sh` — `VISION_KEYS=(VL_URL VL_MODEL)`), and import
  explicitly ignores a carried token so the target’s freshly generated
  sidecar/daemon pair is never split (`ic/scripts/import-tenant.sh:512-522`).
- Test coverage is genuine: middleware unit tests against a mock axum server,
  XML-escaping test, Engine V2 + legacy enrichment matrix in
  `ic/tests/engine_v2_channel_delivery.rs`, and a scoped-auth unit test in
  `wrapper.rs`.

## Findings

### P0-1. Failed encrypted downloads leak the XEP-0454 key URL into content and logs

`extract_inbound_attachments()` returns only successful downloads — failures
log and become `None` (`ic/src/channels/xmpp/mod.rs:2911-2921`).
`strip_attachment_urls()` is then called with only those successful attachments
(`ic/src/channels/xmpp/mod.rs:1538-1539`) and removes only their `source_url`
values (`:2949-2961`).

So when an `aesgcm://...#IV+key` download times out, exceeds the size cap, or
fails GCM authentication, the original key-bearing URL stays in
`IncomingMessage.content` and flows into model context and persisted history.
The same failure path also logs the complete URL — fragment included — at
`:2914` (`tracing::warn!(url = %url, ...)`). For XEP-0454 the fragment *is* the
IV plus the 32-byte media key; for plain HTTPS OOB URLs it can carry signed-URL
query credentials. Both the emitted content and the log line are credential
leaks.

Required fix: track recognized attachment URLs independently of download
success, strip (or redact) every recognized `aesgcm://`/OOB token from content
before the message is emitted, and log only a redacted form (scheme + host, or
a digest). Add regression tests for timeout, oversize, and GCM-tag failure
asserting neither logs nor emitted content contain the fragment.

### P0-2. Inbound OOB/aesgcm downloads have no SSRF or redirect guard

The download client is built with only a timeout
(`ic/src/channels/xmpp/mod.rs:2893-2896`): reqwest defaults apply, meaning up
to 10 redirects are followed, environment proxies are honored, and there is no
scheme/host/resolved-IP filtering. `download_oob_file()` (`:3018-3080`) and
`download_aesgcm_file()` (`:3083+`) fetch arbitrary sender-supplied targets.

Any allowed DM contact — or any participant in an allowed groupchat — can make
the daemon issue authenticated-context HTTP requests to loopback, private,
link-local (e.g. cloud metadata `169.254.169.254`), or internal admin
endpoints, and a redirect can retarget an initially innocent public URL at an
internal address. The streaming size cap bounds memory, not network reach.
This is inconsistent with the hard loopback/no-redirect/no-proxy posture the
same change applies to the LunarVision client.

The proposal (`docs/proposals/XMPP_LUNARVISION_INTEGRATION.md:62-64`) openly
defers "inbound-download SSRF hardening" to XMPP file-transfer item 15, so this
is a known gap rather than a regression — but it remains open and should stay
release-blocking for any build that treats XMPP senders as untrusted.

### P1-1. Sidecar Bearer comparison is not constant-time

`projects/ocr-sidecar/src/main.rs:603-604` compares the provided token with
`provided == token` — an early-exit string compare. On a loopback-only sidecar
the practical risk is low, but a constant-time compare is a one-line
hardening. Related note, not a defect: when `LUNARWING_AUTH_TOKEN` is unset the
sidecar accepts all requests (`:599-600`); acceptable for loopback-only
deployments but worth keeping explicit in the sidecar README.

### P2-1. Scoped credential is matched by tool name

`WasmToolWrapper::new` enables the scoped Bearer credential by matching
hardcoded tool names `"vision-analyze" | "vision-analyze-tool"`
(`ic/src/tools/wasm/wrapper.rs:1057-1071`). A renamed or forked vision tool
silently loses auth (fail-closed, so merely a functional surprise), and any
future tool colliding with those names receives the token. A capabilities-flag
(e.g. `needs_vision_auth` in the tool manifest) would be more robust. Low
priority — current behavior fails safe in both directions.

### P2-2. Per-message clone cost for enrichment

`agent_loop` clones the whole `IncomingMessage` — including raw image bytes
(up to 10 MiB each) — to run enrichment (`ic/src/agent/agent_loop.rs:1486-1491`).
This is a transient allocation spike per image-bearing message, not a
correctness issue. Enriching in place before the Engine V2 routing branch would
avoid the copy; only worth doing if allocation shows up in profiles.

### P2-3. IPv6 loopback scoped-credential injection is untested

`ScopedBearerCredential::new` stores `host_str()` lowercased
(`ic/src/tools/wasm/wrapper.rs:118`), which for `http://[::1]:8088` yields
`::1` without brackets. Whether `extract_host_from_url()` produces the same
string for the request URL is not covered by any test I found. Failure mode is
fail-closed (token simply not injected), so this is a coverage gap, not a
leak — worth one unit test.

## Recommendations

1. Fix P0-1 (strip/redact recognized attachment URLs on all outcomes; redact
   URL logging) — this is the cheapest serious credential-leak fix in the area.
2. Land the item-15 SSRF hardening (P0-2): scheme allowlist, redirect policy
   `none`, resolved-IP filtering against loopback/private/link-local ranges,
   and `no_proxy` for the OOB download client.
3. Constant-time token compare in the sidecar (P1-1).
4. Optional polish: capabilities-based scoped auth (P2-1), avoid the enrichment
   message clone (P2-2), IPv6 scoped-auth unit test (P2-3).

## Verdict

The LunarVision enrichment and scoped-auth work is high quality: its own
trust boundaries are enforced in code, the docs match the implementation, and
the tests are real. What blocks calling the overall XMPP file-sharing +
LunarVision integration hardened is the pre-existing inbound download path:
the key-URL leak on failed downloads (P0-1) and the unguarded sender-controlled
fetches (P0-2). Both are localized to `ic/src/channels/xmpp/mod.rs` and
fixable without touching the new enrichment code.
