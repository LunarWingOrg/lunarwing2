# Desktop Voice Gateway API

LunarWing gateway exposes authenticated transcription and synthesis endpoints for desktop clients. Desktop clients call gateway only; they must never connect directly to LunarVoice.

## Configuration

Gateway enables voice proxy when both variables are present:

```env
VOICE_SERVICE_URL=http://127.0.0.1:8090
VOICE_AUTH_TOKEN=<sidecar bearer token>
```

`VOICE_SERVICE_URL` must use plain HTTP and target `localhost`, `127.0.0.0/8`, or `::1`. Credentials, paths, query strings, fragments, redirects, and proxy environment variables are rejected or disabled. Gateway holds sidecar URL and token server-side and never includes either value in API responses.

Desktop requests use normal gateway bearer authentication:

```http
Authorization: Bearer <gateway token>
Content-Type: application/json
```

Query-string token authentication is not accepted for voice POST endpoints.

## Transcription

`POST /api/voice/transcribe`

```json
{
  "audio": "<base64 audio>",
  "format": "wav",
  "language": "en",
  "model": "whisper-tiny"
}
```

`audio` is required. `format` defaults to `wav`. `language` and `model` are optional and omitted from the sidecar request when absent, allowing sidecar defaults to apply.

```json
{
  "text": "Hello",
  "language": "en",
  "model": "whisper-tiny",
  "confidence": 0.94,
  "segments": []
}
```

Request body limit is 8 MiB including base64 and JSON framing. LunarVoice performs decoded-media validation and enforces `VOICE_MAX_AUDIO_BYTES` and `VOICE_MAX_AUDIO_SECS`; defaults are 50 MiB and 300 seconds. Effective gateway upload capacity is lower because base64 expands binary data.

## Synthesis

`POST /api/voice/synthesize`

```json
{
  "text": "Hello",
  "voice": "af_heart",
  "language": "en",
  "speed": 1.0
}
```

`text` is required and limited to 2,000 Unicode characters. Request body is limited to 4 KiB. `voice` and `language` are optional; `speed` defaults to `1.0` and must be between `0.5` and `2.0`.

```json
{
  "audio": "<base64 WAV>",
  "format": "wav",
  "sample_rate": 24000,
  "duration_ms": 1200,
  "voice": "af_heart"
}
```

Gateway accepts at most 4 MiB from synthesis response. LunarVoice also enforces `VOICE_MAX_TTS_AUDIO_SECS` (300 seconds by default).

## Runtime Contract

- Gateway allows 30 voice requests per authenticated user per 60 seconds. LunarVoice applies its own process-wide inference rate and concurrency limits.
- Gateway connect timeout is 3 seconds; total sidecar request timeout is 120 seconds.
- Disconnecting or cancelling desktop HTTP request drops gateway request future and cancels waiting for sidecar response. Sidecar inference already started may continue until its own timeout.
- Transcription response is limited to 1 MiB. Synthesis response is limited to 4 MiB. Both limits are enforced while streaming, including responses without `Content-Length`.

## Errors

| Status | Meaning |
|---|---|
| `400` | Empty/invalid input or invalid synthesis speed |
| `401` | Missing or invalid gateway bearer token |
| `413` | Gateway request body or declared input limit exceeded |
| `429` | Gateway or LunarVoice rate limit exceeded |
| `500` | Gateway-to-sidecar credential rejected or internal proxy failure |
| `502` | LunarVoice returned an error, invalid JSON, or oversized response |
| `503` | Voice proxy is not configured or sidecar connection failed |
| `504` | LunarVoice request exceeded gateway timeout |

Error bodies contain only gateway-safe messages. Sidecar URL, bearer token, and raw upstream error body are not returned.
