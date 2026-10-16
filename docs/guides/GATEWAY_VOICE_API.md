# Desktop-Facing Gateway Voice API (WI#131)

The web gateway exposes authenticated, bounded voice endpoints that proxy the
loopback LunarVoice sidecar (`projects/voice-sidecar/`). Desktop clients call
the gateway with their normal bearer token; the gateway holds the sidecar
credential server-side (`VOICE_AUTH_TOKEN`) and clients never see it. This is
the LunarWing-side half of desktop goal #19 (`lunarwing/lunarwing2desktop`).

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-28 (v2.1.3.0, WI#259), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), and 2026-10-14 (v2.1.6.0).** The enabling knobs,
loopback-only `VOICE_SERVICE_URL` invariants, default sidecar port (8090), the
422/503/504 status preservation, the synthesis-duration pre-flight (including
the speed-scaled estimate), and the integration test were re-checked against
`ic/src/channels/web/voice.rs` and `ic/tests/voice_gateway_integration.rs`;
no changes were needed (the v2.1.4.0 WI#264 admission-bound sidecar work is
covered in `projects/voice-sidecar/README.md`, not the gateway surface).

## Canonical Documentation

- [`projects/voice-sidecar/README.md`](../../projects/voice-sidecar/README.md) — sidecar API, configuration, deployment
- [`projects/voice-sidecar/PODMAN_DEPLOYMENT.md`](../../projects/voice-sidecar/PODMAN_DEPLOYMENT.md) — sidecar Podman deployment
- `ic/src/channels/web/voice.rs` — gateway proxy, wire types, bounds
- `ic/src/llm/transcription/lunarvoice.rs` — shared loopback endpoint invariants
- `ic/tests/voice_gateway_integration.rs` — end-to-end contract tests

## Enabling

The voice API activates on the gateway when the transcription provider is
`lunarvoice` and the dedicated loopback token is present. It is independent of
`TRANSCRIPTION_ENABLED` (which only gates the agent-side audio-attachment
middleware), so a desktop-facing voice surface can exist without enabling
message transcription.

```bash
# LunarVoice sidecar (loopback only; default http://127.0.0.1:8090)
TRANSCRIPTION_PROVIDER=lunarvoice
VOICE_SERVICE_URL=http://127.0.0.1:8090
VOICE_AUTH_TOKEN=<sidecar-bearer-token>
TRANSCRIPTION_MODEL=whisper-tiny   # model the gateway selects for clients
# Optional: mirror the sidecar's own bounds so the gateway pre-flight and
# proxy timeout track what the sidecar enforces (mt-admin propagates both
# from the tenant voice.env automatically).
VOICE_MAX_TTS_AUDIO_SECS=300
VOICE_REQUEST_TIMEOUT_SECS=120
```

The gateway's proxy timeout is the sidecar's `VOICE_REQUEST_TIMEOUT_SECS` plus
fixed headroom (30 s), so the sidecar's own structured timeout response always
reaches the client instead of the gateway racing it.

`VOICE_SERVICE_URL` must be plain HTTP, point at `localhost` or a loopback IP,
and carry no credentials, path, query, or fragment — the same invariants the
agent-side transcription provider enforces. The gateway refuses to construct
the proxy for anything else.

## Gateway Endpoints

All routes sit behind the standard gateway auth middleware (bearer token from
`GATEWAY_AUTH_TOKEN`, `GATEWAY_USER_TOKENS`, or DB-backed tokens) and share the
gateway's CORS and security-header layers.

| Endpoint | Method | Description |
| --- | --- | --- |
| `/api/voice/status` | GET | Capability discovery: `configured`, `model`, bounds |
| `/api/voice/transcribe` | POST | Base64 audio → text (STT) |
| `/api/voice/synthesize` | POST | Text → base64 WAV (TTS) |

### GET /api/voice/status

```json
{
  "configured": true,
  "model": "whisper-tiny",
  "max_audio_b64_len": 25165824,
  "max_text_chars": 10000,
  "max_synth_audio_secs": 300
}
```

`max_synth_audio_secs` mirrors the sidecar's `VOICE_MAX_TTS_AUDIO_SECS` clip
cap: text whose estimated speech duration exceeds it is rejected up front with
422 `text_too_long`, so desktop clients can chunk or trim long agent replies
before sending them.

### POST /api/voice/transcribe

```json
{
  "audio": "<base64 audio bytes>",
  "format": "wav",
  "language": "auto"
}
```

`format` is one of `wav`, `ogg`, `opus`, `mp3`, `mp4`, `webm`, `flac`, `m4a`,
`aac` (default `wav`); `language` is an optional hint (max 32 chars). The
response mirrors the sidecar contract:

```json
{
  "text": "hello from the sidecar",
  "language": "en",
  "confidence": 0.9,
  "segments": [{ "start_ms": 0, "end_ms": 500, "text": "hello from the sidecar" }],
  "model": "whisper-tiny"
}
```

### POST /api/voice/synthesize

```json
{
  "text": "hello",
  "voice": "af_heart",
  "speed": 1.0
}
```

Response:

```json
{
  "audio": "<base64 WAV bytes>",
  "format": "wav",
  "sample_rate": 24000,
  "duration_ms": 1000,
  "voice": "af_heart"
}
```

### Errors

Errors use a stable JSON shape. Upstream (sidecar) failures are never
forwarded verbatim — the sidecar's `detail` text is logged server-side and
clients receive a generic `upstream_error` message, so a sidecar credential
failure reads as a server problem, not a gateway-token problem.

```json
{ "error": "bad_request", "message": "speed must be between 0.5 and 2.0" }
```

| `error` | HTTP | Meaning |
| --- | --- | --- |
| `bad_request` | 400 | Validation failure (format, base64, text/voice/speed/language bounds) |
| `unauthorized` | 401 | Missing/invalid gateway bearer token (auth middleware) |
| `payload_too_large` | 413 | Base64 audio exceeds `max_audio_b64_len` |
| `text_too_long` | 422 | Estimated audio duration exceeds `max_synth_audio_secs` (rejected before the sidecar runs) |
| `rate_limited` | 429 | Per-user voice rate limit or sidecar rate limit |
| `processing_failed` | 422 | Sidecar could not process the request (sidecar 422, e.g. synthesized audio exceeded its cap) |
| `unavailable` | 503 | Voice API not configured, or sidecar reports unavailable (sidecar 503) |
| `upstream_error` | 500 | Sidecar rejected the gateway credential (sidecar 401) |
| `upstream_error` | 502 | Other sidecar failure (unreachable, malformed/oversize response) |
| `upstream_timeout` | 504 | Sidecar request timed out (sidecar 504, or the gateway proxy budget — always strictly larger than the sidecar's own — expired) |

The sidecar's documented processing/overload statuses (422/503/504) keep their
semantics end-to-end (WI#259): a desktop client can distinguish "this reply is
too long to synthesize" from "the sidecar is down", which previously collapsed
into one opaque 502.

**Sidecar admission and recovery (WI#264, v2.1.4.0):** a 504 ends the wait,
not an already-running native synthesis. The sidecar admits one call per
component before consuming shared capacity, so subsequent TTS calls receive
503 while it drains instead of queuing behind its mutex. STT/VAD can use
remaining permits (none may remain with a concurrency limit of one). Normal
completion restores admission; a poisoned model/panic or a never-returning
call requires operator recovery. The sidecar's `/ready` and `/metrics` expose
per-component `inference` state, and `/ready` stays 503 while abandoned work
drains or a component is failed. The gateway preserves HTTP 503 as generic
`unavailable`, not the sidecar's `component_failed` detail; retry with backoff
for transient busy responses, and escalate persistent failures to an operator.
See [sidecar recovery](../../projects/voice-sidecar/README.md#inference-timeout-admission-and-recovery-wi264)
for the supported `restart-tenant` procedure (whole-tenant interruption).

## Bounds and Security

- **Server-held credential**: only the gateway knows `VOICE_AUTH_TOKEN`; the
  sidecar sees it on the proxied request, the desktop client never does.
- **Loopback-only upstream**: proxy endpoints are derived from `VOICE_SERVICE_URL`
  through the shared `voice_endpoint` builder (HTTP, loopback host, no
  credentials/query/fragment/path). `no_proxy` + no-redirect client.
- **Request bounds**: base64 audio capped at 24 MiB (≈18 MiB decoded), synthesis
  text at 10 000 chars, voice id at 64 chars, speed in 0.5..=2.0, language at
  32 chars, `format` allowlisted. Synthesis text additionally passes a
  conservative duration pre-flight (Latin ≈14 chars/s, CJK ≈3.5 chars/s,
  scaled by the requested speed the same way the sidecar's engine scales the
  clip) against `VOICE_MAX_TTS_AUDIO_SECS`, rejecting guaranteed over-cap
  clips before any sidecar CPU is spent. Per-route body limits
  (32 MiB transcribe, 1 MiB synthesize) override the gateway's global 10 MiB
  limit.
- **Response caps**: sidecar responses are streamed with hard byte caps
  (1 MiB transcription JSON, 64 MiB synthesis audio) before re-serialization.
  Synthesis responses must declare `format: "wav"` — anything else is rejected
  as an upstream contract violation rather than mislabeled audio.
- **Rate limiting**: per-user `10 requests / 60 s` across voice endpoints, in
  addition to the sidecar's own rate limiter.
- **Client model is fixed**: clients cannot choose the STT model; the gateway
  selects `TRANSCRIPTION_MODEL` on their behalf.

## Desktop Client Flow

1. `GET /api/voice/status` — check `configured` (and read
   `max_synth_audio_secs`) before showing voice UI.
2. Record audio → base64 → `POST /api/voice/transcribe`.
3. Agent text → chunk/trim against `max_synth_audio_secs` →
   `POST /api/voice/synthesize` → play returned base64 WAV. Over-long text is
   rejected with 422 `text_too_long` (the estimate scales with the requested
   `speed`, so a faster playback setting admits longer text); chunk at
   sentence boundaries and synthesize sequentially.

## Testing

```bash
# Unit tests (validation, endpoint invariants, proxy mapping)
taskset -c 0-11 cargo test -j12 --lib -- voice:: lunarvoice

# End-to-end (real gateway + mock sidecar, auth + bounds + credential flow)
taskset -c 0-11 cargo test -j12 --test voice_gateway_integration
```
