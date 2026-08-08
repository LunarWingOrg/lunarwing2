# LunarVoice Sidecar

LunarVoice provides local ONNX speech-to-text, text-to-speech, and voice
activity detection for LunarWing. It runs outside the daemon, exposes a bounded
loopback HTTP API, and uses `sherpa-rs`/sherpa-onnx for inference.

## Security Defaults

- API bearer token required unless `VOICE_ALLOW_UNAUTHENTICATED=true` is set.
- Bare-metal bind defaults to `127.0.0.1`; container manifests override internal
  bind to `0.0.0.0` while publishing both host ports on loopback only.
- Health listener is separate and exposes only `/health`, `/ready`, and `/metrics`.
- Request bodies, decoded bytes, audio duration, text length, and rate are bounded.
- Blocking inference concurrency, processing time, and synthesized duration are bounded.
- Redirects and arbitrary outbound requests are not used by the service.
- Non-WAV decoding invokes configured `ffmpeg` directly with fixed arguments,
  never through a shell.
- Model auto-download is off. When enabled, every HTTPS artifact needs an exact
  SHA-256 and archive paths are checked before extraction.

## API

API defaults to `127.0.0.1:8090` on the host. Every API request needs
`Authorization: Bearer $VOICE_AUTH_TOKEN`.

```text
POST /v1/voice/transcribe  {audio, format, language, model}
POST /v1/voice/synthesize  {text, voice, language, speed}
POST /v1/voice/detect      {audio, format}
```

`audio` is standard base64. Supported format labels are `wav`, `ogg`, `opus`,
`mp3`, `mp4`, `webm`, `flac`, `m4a`, and `aac`. WAV is decoded in-process;
other formats require `ffmpeg`.

Health defaults to `127.0.0.1:8091`:

```text
GET /health   process liveness and loaded components
GET /ready    STT readiness; returns 503 until required models load
GET /metrics  bounded operational counters
```

## Models

LunarVoice expects sherpa-onnx model layouts:

```text
/models/sherpa-onnx-whisper-tiny/
  tiny-encoder.int8.onnx
  tiny-decoder.int8.onnx
  tiny-tokens.txt
/models/silero_vad.onnx
/models/kokoro-multi-lang-v1_0/
  model.onnx
  voices.bin
  tokens.txt
  espeak-ng-data/
  dict/
  lexicon-us-en.txt
  lexicon-zh.txt
```

### STT engines

`VOICE_STT_ENGINE` selects the speech-to-text engine:

- `whisper` (default) — Whisper via sherpa-onnx, multilingual, reports the
  detected language per chunk. Default model `whisper-tiny`.
- `parakeet` — NVIDIA Parakeet-TDT via the sherpa-onnx transducer
  recognizer. Supported models: `parakeet-tdt-0.6b-v2-int8` (English) and
  `parakeet-tdt-0.6b-v3-int8` (multilingual). Set `VOICE_STT_MODEL` to one of
  those names; the expected layout is:

```text
/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/
  encoder.int8.onnx
  decoder.int8.onnx
  joiner.int8.onnx
  tokens.txt
```

Parakeet does not detect the spoken language; the response language is the
configured `VOICE_STT_LANGUAGE` (set it explicitly, e.g. `en` for the v2
model). Explicit `VOICE_STT_ENCODER`/`VOICE_STT_DECODER`/`VOICE_STT_JOINER`/
`VOICE_STT_TOKENS` paths override the derived locations for either engine.

Models are not committed or silently downloaded. Copy
`model-manifest.example.json`, replace each placeholder with a SHA-256 obtained
through a trusted release channel, mount it, then set:

```env
VOICE_AUTO_DOWNLOAD=true
VOICE_MODEL_MANIFEST=/models/model-manifest.json
```

The downloader refuses HTTP, malformed hashes, unsafe archive paths, and
replacement of unverified existing model directories.

## Build And Test

Use repository resource limits and tmux:

```bash
tmux new-session -d -s lunarvoice-test \
  "taskset -c 0-5 cargo test -j6 2>&1 | tee /tmp/lunarvoice-test.log"
```

See `PODMAN_DEPLOYMENT.md` and `.env.example` for deployment.
