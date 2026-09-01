# XMPP to LunarVision Integration

> **Current status (updated 2026-08-31): IMPLEMENTED.** Inbound XMPP images are
> automatically enriched through the configured per-tenant LunarVision
> instance—local or external HTTPS—before either legacy agent handling or
> Engine V2 attachment routing.

## Implemented flow

1. Native or WASM XMPP handling reconstructs an `IncomingAttachment` with
   inline image bytes.
2. After inbound hooks accept ordinary user input, the supervised message task
   invokes `VisionEnrichmentMiddleware` when `VISION_SERVICE_URL` is configured.
3. Middleware accepts only XMPP image attachments, posts at most two requests
   concurrently to the configured `/v1/vision/analyze` endpoint, and applies
   `VISION_AUTH_TOKEN` as a host-side Bearer credential when configured. Local
   instances may use HTTP; remote instances require HTTPS.
4. Bounded OCR/VL output is stored in `IncomingAttachment.extracted_text` and
   rendered as explicitly untrusted attachment content for both agent engines.
5. Original image bytes remain unchanged and still reach a vision-capable LLM
   through existing multimodal content parts.

This is agent-loop middleware rather than a `BeforeInbound` hook because the
hook contract carries text only and cannot inspect or mutate attachments. It
runs after hooks and inside the normal message timeout so slow VL inference does
not block channel receive, shutdown, or deferred-message handling.

## Failure behavior

LunarVision errors, timeouts, invalid JSON, oversized output, unsupported image
formats, and `4xx`/`5xx` responses fail open: no extracted text is added, but the
original image remains available to provider-native multimodal handling.
Images above the decoded 10 MiB LunarVision limit skip enrichment while keeping
the existing XMPP attachment path.

## Security properties

- Service URLs must be origins without credentials, paths, query strings, or
  fragments. HTTP is accepted only for loopback and `host.containers.internal`;
  remote instances require HTTPS. Redirects and environment proxies remain
  disabled. An incompatible URL disables automatic enrichment without
  preventing daemon startup or the separately sandboxed explicit vision tool
  from loading.
- Bearer credentials use `SecretString`, are injected only on the host request,
  and never enter attachment text, LLM parameters, or logs.
- The explicit `vision-analyze` WASM tool receives the same host-managed token
  only for the exact configured origin and `/v1/` path. The token is never
  serialized into WASM context; host capability and request-time checks both
  reject a different scheme, host, port, or path. Proxies are bypassed only for
  local service origins, while external HTTPS can use normal proxy policy.
- User message text is not sent to LunarVision. The accepted attachment is
  analyzed with a fixed prompt; normal message text continues through inbound
  safety checks and the selected agent engine.
- Sidecar responses are capped at 1 MiB; extracted text is capped at 100,000
  bytes and XML-escaped before model context insertion.
- Raw `aesgcm://` source URLs and image base64 remain absent from persisted text.

## Verification

Focused tests cover authenticated request construction, fixed-prompt isolation,
direct and cached sidecar response shapes, non-XMPP exclusion, fail-open
fallback, local-HTTP/remote-HTTPS URL enforcement, XML escaping, decoded image
limits, and VL data-URL construction. Host credential tests cover exact-origin
scoping for the explicit WASM vision tool, including scheme, host, port, and
versioned-path rejection.

Live Conversations/Gajim/Dino transfer validation remains tracked by XMPP
file-transfer item 15. Inbound-download SSRF hardening (formerly deferred to
item 15) has since landed: inbound OOB/`aesgcm://` fetches go through an
egress policy (https-only by default, no embedded credentials, redirects and
environment proxies disabled, non-public resolved addresses rejected, client
pinned to validated addresses, `ALLOW_PRIVATE_IPS=1` local-dev opt-in), and
recognized attachment URLs are stripped from message content and logs even
when a download fails (2026-08-01, branch `rarity/kimi-27-20260801-0000`).

Kawarimi migration carries local-sidecar `VL_URL`/`VL_MODEL` settings but not an
external instance URL/token or deployment mode. Imports therefore retain the
target's matching local sidecar/daemon credential pair. Operators re-run
`configure-vision-external` after migration when the destination should use the
remote service.
