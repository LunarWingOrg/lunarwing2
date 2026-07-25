# XMPP to LunarVision Integration

> **Current status (2026-07-25): IMPLEMENTED.** Inbound XMPP images are
> automatically enriched through the configured per-tenant LunarVision
> sidecar before either legacy agent handling or Engine V2 attachment routing.

## Implemented flow

1. Native or WASM XMPP handling reconstructs an `IncomingAttachment` with
   inline image bytes.
2. After inbound hooks accept ordinary user input, the supervised message task
   invokes `VisionEnrichmentMiddleware` when `VISION_SERVICE_URL` is configured.
3. Middleware accepts only XMPP image attachments, posts at most two requests
   concurrently to the loopback-only `/v1/vision/analyze` endpoint, and applies
   `VISION_AUTH_TOKEN` as a host-side Bearer credential when configured.
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

- Service URLs must use plain HTTP on `localhost` or a loopback IP. Paths,
  credentials, query strings, fragments, non-loopback hosts, redirects, and
  environment proxies are rejected or disabled. An incompatible URL disables
  automatic enrichment without preventing daemon startup or the separately
  sandboxed explicit vision tool from loading.
- Bearer credentials use `SecretString`, are injected only on the host request,
  and never enter attachment text, LLM parameters, or logs.
- The explicit `vision-analyze` WASM tool receives the same host-managed token
  only for the configured sidecar origin and `/v1/` path. The token is never
  serialized into WASM context, redirects and proxies are disabled for that
  scoped request, and another local port cannot receive it.
- User message text is not sent to LunarVision. The accepted attachment is
  analyzed with a fixed prompt; normal message text continues through inbound
  safety checks and the selected agent engine.
- Sidecar responses are capped at 1 MiB; extracted text is capped at 100,000
  bytes and XML-escaped before model context insertion.
- Raw `aesgcm://` source URLs and image base64 remain absent from persisted text.

## Verification

Focused tests cover authenticated request construction, fixed-prompt isolation,
direct and cached sidecar response shapes, non-XMPP exclusion, fail-open
fallback, loopback URL enforcement, XML escaping, decoded image limits, and VL
data-URL construction. Host credential tests also cover exact-origin scoping for
the explicit WASM vision tool.

Live Conversations/Gajim/Dino transfer validation and inbound-download SSRF
hardening remain tracked by XMPP file-transfer item 15; they are not part of
this now-closed automatic enrichment gap.

Kawarimi migration carries `VL_URL`/`VL_MODEL` but deliberately regenerates the
host-local vision auth token. Imports of older bundles ignore their carried
sidecar token, preserving the target's matching sidecar/daemon credential pair.
