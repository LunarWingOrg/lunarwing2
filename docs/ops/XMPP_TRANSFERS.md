# XMPP File Transfer Methods — Quick Reference

## Update 2026-07-25

Inbound XMPP images now pass through automatic LunarVision OCR/VL enrichment
when `VISION_SERVICE_URL` is configured. Enrichment uses the per-tenant
`VISION_AUTH_TOKEN` when configured, fails open to the existing multimodal image path, and never
persists raw image data or encrypted-media source URLs in text history.

### docs/ops/XMPP_TRANSFERS.md

*Created 2026-06-18, Kestrel*

## Overview

XMPP supports multiple file transfer methods, each with different tradeoffs in terms of server requirements, privacy, and file size limits.

---

## Protocol Comparison

| Protocol | XEP | Method | Server Required | File Size | Encryption |
|----------|-----|--------|-----------------|-----------|------------|
| **HTTP File Upload** | XEP-0363 | Server-mediated | Yes (upload slot) | Limited by server | OMEMO + HTTPS |
| **Jingle File Transfer** | XEP-0234 | Peer-to-peer | No (NAT traversal) | Unlimited | OMEMO + DTLS |
| **Out-of-Band Data** | XEP-0066 | External URL | No | Unlimited | Depends on URL |
| **In-Band Bytestreams** | XEP-0361 | Base64 in stanza | No | Small only (<1MB) | OMEMO |

---

## Implementation Status (LunarWing XMPP Bridge)

### ✅ Implemented

| Feature | Status | Notes |
|---------|--------|-------|
| `jabber:x:oob` (XEP-0066) | ✅ Implemented | Parses `<x xmlns='jabber:x:oob'>` elements |
| `aesgcm://` URLs (OMEMO) | ✅ Implemented | AES-256-GCM decryption for encrypted media |
| HTTP File Upload URLs (XEP-0363) | ✅ Implemented | Handles server-generated upload URLs |
| Bounded concurrency | ✅ Implemented | 4 concurrent downloads max |
| Size cap | ✅ Implemented | 20MB limit per file |
| Unit tests | ✅ Implemented | `ic/src/channels/xmpp/mod.rs` |
| Automatic LunarVision enrichment | ✅ Implemented | Host-side Bearer auth when configured; OCR/VL text plus original image |

### ⏳ Pending

| Feature | Status | Notes |
|---------|--------|-------|
| End-to-end validation | ⏳ Pending | Live testing with Conversations/Gajim |
| Inbound download SSRF guard | ⏳ Deferred | Network boundary relied upon for now |

### ❌ Not Implemented

| Feature | Status | Notes |
|---------|--------|-------|
| Jingle File Transfer (XEP-0234) | ❌ Not in scope | P2P direct connection — requires ICE/STUN/TURN |
| In-Band Bytestreams (XEP-0361) | ❌ Not in scope | Base64 in stanzas — inefficient for files |

---

## Architecture

### Inbound File Transfer Flow

```
XMPP Client → XMPP Bridge → Agent
    │              │            │
    │  1. Extract URLs (oob/aesgcm/XEP-0363)
    │  2. Download (concurrent, capped)
    │  3. Decrypt (if aesgcm://)
    │  4. Base64 encode
    │  5. Attach to IncomingMessage
    │              │            │
    │              └───────────→│ 6. Process attachment
    │                           │ 7. LunarVision OCR/VL enrichment (images, if configured)
    │                           │ 8. Preserve original image for multimodal LLM input
```

### Code Locations

- **URL extraction:** `ic/src/channels/xmpp/mod.rs` — `extract_inbound_attachments()`
- **OOB parsing:** `Oob::try_from` (XEP-0066)
- **AES-GCM decryption:** `download_aesgcm_file()`
- **HTTP download:** `download_oob_file()` (with `read_capped_body`)
- **Bridge message:** `BridgeMessage` (base64-encoded attachments)
- **WASM channel:** `on_poll()` decodes and calls `store_attachment_data()`
- **LunarVision enrichment:** `ic/src/vision_enrichment.rs`
- **Agent integration:** supervised user-input handling in `ic/src/agent/agent_loop.rs`

---

## Testing Checklist

### Phase 1: Unit Tests (✅ Complete)
- [x] OOB URL parsing
- [x] AES-GCM decryption
- [x] HTTP download with size cap
- [x] Concurrent download limits
- [x] Authenticated LunarVision request and OCR/VL context injection
- [x] LunarVision failure preserves provider-native multimodal fallback
- [x] Explicit `vision-analyze` tool auth scoped to configured sidecar origin

### Phase 2: End-to-End Validation (⏳ Pending)
- [ ] Test with Conversations (Android)
- [ ] Test with Gajim (Linux)
- [ ] Test with Dino (Linux)
- [ ] Verify OMEMO-encrypted file transfers
- [ ] Verify HTTP Upload URL handling
- [ ] Verify OOB URL handling

### Phase 3: SSRF Hardening (⏳ Deferred)
- [ ] Implement allowlist for download domains
- [ ] Add network isolation for download workers
- [ ] Document security tradeoffs

---

## Prosody XEP-0363 Configuration (Reference)

For HTTP File Upload support, Prosody requires `mod_http_upload`:

```lua
-- Prosody config (prosody.cfg.lua)
Component "files.example.com" "http_upload"
    file_limit = 10485760  -- 10MB
    external_component_secret = "your-secret"
```

**Known Issue:** XEP-0363 module setup requires proper slot provisioning and CORS configuration. See `docs/bugs/` for troubleshooting notes.

---

## Related Documentation

- [`../architecture/XMPP_FILE_TRANSFERS.md`](../architecture/XMPP_FILE_TRANSFERS.md) — full implementation details
- [`../bugs/history/XMPP-OMEMO-BUG-TO-DO.md`](../bugs/history/XMPP-OMEMO-BUG-TO-DO.md) — archived OMEMO bug tracker
- [`history/AGENT_GOALS_2.0.3.0.md`](history/AGENT_GOALS_2.0.3.0.md) — archived release goals that included XMPP polish
- [`../../ic/testing/lunarwing-xmpp/README.md`](../../ic/testing/lunarwing-xmpp/README.md) — test harness setup

---

## Current Status

**Implemented:**
- Inbound file transfer support (OOB, AES-GCM, HTTP Upload URLs)
- Bounded concurrency (4 downloads max)
- 20 MB streamed file-size cap
- Inbound URL egress policy: HTTPS by default, public-address validation and
  DNS pinning, no redirects or environment proxies; `ALLOW_PRIVATE_IPS=1` is
  the explicit private-network opt-out

**Remaining:**
- Live end-to-end validation against a real XMPP server and client

**Not Included:**
- Jingle File Transfer (XEP-0234) — out of scope

---

*Glory is for admirals. Uptime is forever.*
