# Proposal: LunarVoice — ONNX-Based Voice Processing for LunarWing

## Target Release: v2.0.6.0

## Status: IMPLEMENTED for v2.0.6.0

## Implementation Notes

Implementation preserves proposal goals but corrects assumptions found during source review:

- LunarWing already had cloud audio transcription through
  `ic/src/llm/transcription/`. LunarVoice implements that existing
  `TranscriptionProvider` contract instead of adding parallel enrichment middleware.
- Sidecar uses Axum plus `sherpa-rs` 0.6.8 (sherpa-onnx) for Whisper, Silero VAD,
  and Kokoro. This avoids duplicating model-specific preprocessing with direct `ort`.
- API and health listeners use separate defaults: `8090` and `8091`. Proposed
  `8089` conflicted with LunarVision health deployment.
- Multi-tenant host ports come from registry v15 `voice_service` and
  `voice_health` fields in isolated range `40000-49999`.
- Voice is provisioned disabled. `lunarwing-mt-admin.sh enable-voice <tenant>`
  requires verified Whisper artifacts before enabling local transcription.
- Model downloads are opt-in and require operator-supplied exact SHA-256 values.
  No unverified first-run download occurs.
- Health discovery probes only tenants with `enable_voice=true`; LunarVoice is
  monitor-only in self-heal because restarting optional model workloads
  automatically is unsafe.

## Motivation

LunarWing has LunarVision (OCR/image analysis) but no voice processing capability. Agents cannot transcribe audio messages, generate speech responses, or do voice activity detection. As the herd grows and messaging platforms increasingly use voice notes, this becomes a real gap.

ONNX Runtime enables local, private, hardware-accelerated voice AI without cloud dependencies. No API costs, no rate limits, no data leaving the machine — same philosophy as everything else in LunarWing.

## Current State

```
LunarVision (exists):
  projects/ocr-sidecar/          → Rust HTTP server (warp), port 8088
  ic/src/vision_enrichment.rs    → daemon-side enrichment caller
  Uses: Tesseract + optional PaddleOCR + external VL model API
  Deploy: systemd/OpenRC service or rootless Podman container

Audio transcription (exists):
  ic/src/llm/transcription/       → provider trait + inbound middleware
  Cloud providers                → OpenAI Whisper and Chat Completions

LunarVoice (implemented here):
  projects/voice-sidecar/        → local STT, TTS, and VAD
  ic/src/llm/transcription/lunarvoice.rs → loopback provider
  ic/tools-src/voice-transcribe/ → sandboxed WASM tool
```

## Proposed Architecture

Follow the proven LunarVision sidecar pattern — separate process, HTTP API, health endpoint, Podman-deployable.

```
LunarVoice Sidecar (new):
  projects/voice-sidecar/          → Rust HTTP server (Axum), API 8090, health 8091
  ├── STT endpoint: POST /v1/voice/transcribe
  ├── TTS endpoint: POST /v1/voice/synthesize
  ├── VAD endpoint: POST /v1/voice/detect
  ├── Health: GET /health, GET /ready
  └── Auth: bearer token (same pattern as OCR sidecar)

  Backed by:
  ├── ort crate (ONNX Runtime 1.28, load-dynamic feature)
  ├── Silero VAD v5 (ONNX, ~2MB model)
  ├── STT engine (configurable, see below)
  └── TTS engine (configurable, see below)

Daemon Integration:
  ic/src/llm/transcription/lunarvoice.rs → existing transcription pipeline provider
  ic/src/tools/wasm/wrapper.rs     → inject scoped sidecar URL and bearer token
  Health checks: lunarwing-voice-<t> units (same pattern as vision)
```

## ONNX Runtime Integration

### Runtime: sherpa-rs / sherpa-onnx

Final implementation uses `sherpa-rs` with downloaded, checksum-pinned
sherpa-onnx runtime binaries. Direct `ort` integration below records original
design research but is not shipped because sherpa-rs owns required audio/model
preprocessing and exposes all three engines behind one API.

- v2.0.0-rc.13, wraps ONNX Runtime 1.28
- 14.5M downloads, actively maintained (updated Jul 28 2026)
- `load-dynamic` feature: runtime linking, no compile-time ONNX dependency
- Execution providers: CPU (default), CUDA, CoreML, DirectML, Vulkan, TensorRT, ROCm
- Used by: FastEmbed-rs, YOLO Rust, PaddleOCR-rs, Magika

```toml
[dependencies]
ort = { version = "2.0.0-rc.13", features = ["load-dynamic"] }
```

`load-dynamic` means LunarWing doesn't link ONNX Runtime at compile time. The sidecar loads `libonnxruntime.so` at startup from a configured path. If ONNX isn't installed, the sidecar fails closed with a clear error — doesn't affect the main daemon.

## Voice Components

### 1. Voice Activity Detection (VAD) — Silero v5

| Property | Value |
|---|---|
| Model | Silero VAD v5 |
| Size | ~2MB ONNX |
| Purpose | Detect speech segments in audio stream |
| Latency | <10ms per frame |
| Use case | Pre-filter for STT — don't waste inference on silence |

Silero VAD is the standard for on-device VAD. Tiny model, fast inference, well-supported in ONNX format. Used by vox, sherpa-onnx, and most local voice pipelines.

### 2. Speech-to-Text (STT)

Three options, configurable per deployment:

**Option A: Whisper (via sherpa-onnx)**
- Models: tiny (75MB) → medium (1.5GB)
- Multilingual, well-tested
- Offline (non-streaming) — fine for voice notes
- Best accuracy-to-size ratio

**Option B: SenseVoice (via sherpa-onnx)**
- Model: ~230MB int8
- Multilingual (zh/en/ja/ko/yue)
- Faster than Whisper, lower accuracy
- Good for real-time/streaming use cases

**Option C: Moonshine (via sherpa-onnx)**
- Models: base/tiny/small/medium
- English-focused, streaming capable
- Good for English-only deployments

**Recommendation**: Default to Whisper tiny (75MB) for general use. Allow configuration to swap models. Use sherpa-onnx as the unified engine since it supports all three.

### 3. Text-to-Speech (TTS)

Three options, configurable:

**Option A: Kokoro (ONNX)**
- Model: ~310MB (or 77MB int8)
- 57 voices, 9 languages, 24kHz output
- Best quality-to-size ratio
- Recommended default

**Option B: Piper (ONNX)**
- Model: ~63MB per voice
- Multilingual, very fast CPU synthesis
- Good for low-resource deployments (Pi5, N150)

**Option C: Pocket (ONNX)**
- Model: ~82MB
- Pure Rust inference path available
- Edge/embedded optimized

**Recommendation**: Default to Kokoro int8 (77MB). Allow Piper as a lightweight alternative for Volta's Pi5 or Kageho's N150.

### Crate Choice: sherpa-onnx vs transcribe-rs vs direct ort

| Approach | Pros | Cons |
|---|---|---|
| **sherpa-onnx** | Full pipeline (STT+TTS+VAD+diarization), production-tested, C++ core | Large dependency, C++ build requirement |
| **transcribe-rs** | Clean Rust API, STT only, simple | No TTS or VAD, would need separate TTS crate |
| **Direct ort + models** | Minimal deps, full control | Most implementation work, reinventing wheels |

**Recommendation**: Use `sherpa-onnx` Rust bindings. It covers the entire voice pipeline through one dependency. The C++ build requirement is manageable — it ships pre-built for major platforms and the sidecar is already a separate process that can be containerized.

## Sidecar Design

```
projects/voice-sidecar/
├── Cargo.toml
├── src/
│   ├── main.rs              → HTTP server (warp), config, auth, health
│   ├── stt.rs               → STT engine (sherpa-onnx wrapper)
│   ├── tts.rs               → TTS engine (sherpa-onnx wrapper)
│   ├── vad.rs               → VAD engine (Silero via ort)
│   ├── cache.rs             → response caching (same pattern as OCR sidecar)
│   └── config.rs            → model paths, engine selection, ports
├── systemd/
│   └── voice-sidecar.service
├── init.d/
│   └── voice-sidecar        → OpenRC service
├── PODMAN_DEPLOYMENT.md
└── models/                  → .gitignored, downloaded on first run
    ├── silero_vad.onnx
    ├── whisper-tiny.onnx
    └── kokoro-int8.onnx
```

### API Endpoints

```
POST /v1/voice/transcribe
  Body: { "audio": "<base64 wav>", "language": "auto", "model": "whisper-tiny" }
  Response: { "text": "...", "language": "en", "confidence": 0.95, "segments": [...] }

POST /v1/voice/synthesize
  Body: { "text": "...", "voice": "af_heart", "language": "en" }
  Response: { "audio": "<base64 wav>", "sample_rate": 24000, "duration_ms": 3200 }

POST /v1/voice/detect
  Body: { "audio": "<base64 wav>" }
  Response: { "segments": [{ "start_ms": 100, "end_ms": 2500 }], "has_speech": true }

GET /health → { "status": "ok", "uptime_s": 3600, "models_loaded": ["vad", "stt", "tts"] }
GET /ready  → { "ready": true, "stt_engine": "whisper-tiny", "tts_engine": "kokoro-int8" }
```

### Configuration

```env
# Voice sidecar config
VOICE_PORT=8090
VOICE_HEALTH_PORT=8091
VOICE_AUTH_TOKEN=<bearer token>

# Model selection
VOICE_VAD_MODEL=silero_vad_v5.onnx
VOICE_STT_MODEL=whisper-tiny.onnx
VOICE_STT_ENGINE=whisper        # whisper | sensevoice | moonshine
VOICE_TTS_MODEL=kokoro-int8.onnx
VOICE_TTS_ENGINE=kokoro          # kokoro | piper | pocket
VOICE_TTS_DEFAULT_VOICE=af_heart

# Performance
VOICE_MAX_AUDIO_SECS=300
VOICE_RATE_LIMIT_PER_SEC=5
VOICE_CACHE_TTL_SECS=600
VOICE_CACHE_MAX_ENTRIES=5000

# ONNX Runtime
ORT_LIB_PATH=/usr/lib/libonnxruntime.so
ORT_NUM_THREADS=4
ORT_INTER_OP_THREADS=1
```

## Daemon Integration

### Audio Message Transcription

Same pattern as `vision_enrichment.rs` — when an inbound message contains audio (XMPP voice note, etc.):

```rust
// ic/src/voice_enrichment.rs
// 1. Download audio attachment
// 2. POST to voice sidecar /v1/voice/transcribe
// 3. Prepend transcription to message content
// 4. Agent receives text instead of raw audio
```

### Voice Response (optional, per-tenant)

Agent generates text → voice sidecar synthesizes → audio sent back through XMPP/channel. Configurable per-tenant, off by default.

### Tool Integration

WASM tool `vision-analyze` already calls the OCR sidecar at 8088. Add a `voice-transcribe` WASM tool calling 8089, same pattern.

### Health Checks

```bash
# Same pattern as lunarvision health:
lunarwing-voice-<tenant>     # systemd unit
voice-sidecar.service        # standalone systemd
voice-sidecar                # OpenRC service
```

Health registry publishes `voice_health` port (like `vision_health`). Self-heal monitors it.

## Deployment

### Podman (recommended)

```bash
podman run -d \
  --name lunarwing-voice-sidecar \
  -p 8089:8089 \
  -v voice_models:/models \
  -v voice_cache:/app/cache \
  -e VOICE_AUTH_TOKEN=<token> \
  -e ORT_LIB_PATH=/usr/lib/libonnxruntime.so \
  ghcr.io/lunarwing/voice-sidecar:latest
```

Container includes sherpa-onnx runtime libraries but no models. Operators mount
verified models; optional first-run installation requires HTTPS plus exact
SHA-256 values in `VOICE_MODEL_MANIFEST`.

### Bare metal (Gentoo/OpenRC)

```bash
sudo cp init.d/voice-sidecar /etc/init.d/
sudo chmod +x /etc/init.d/voice-sidecar
sudo rc-update add voice-sidecar default
sudo rc-service voice-sidecar start
```

Requires `dev-libs/onnxruntime` or manually placed `libonnxruntime.so`.

## Model Management

First-run downloads models from HuggingFace:

| Model | Source | Size |
|---|---|---|
| Silero VAD v5 | github.com/snakers4/silero-vad | ~2MB |
| Whisper tiny | huggingface.co/openai/whisper-tiny (ONNX) | ~75MB |
| Kokoro int8 | huggingface.co/hexgrad/Kokoro-82M (ONNX int8) | ~77MB |

Total default model footprint: ~154MB. Configurable to larger models if hardware allows.

Models cached in mounted volume, persistent across container restarts.

## Resource Considerations

| Hardware | STT Latency | TTS Latency | Feasible? |
|---|---|---|---|
| Ryzen 9 (dev VM) | <1s for 30s audio | <2s for sentence | Excellent |
| N150 (ninja fortress) | ~3s for 30s audio | ~5s | Usable (tiny models) |
| Pi5 (Volta) | ~5s for 30s audio | ~8s | Usable (Piper TTS) |
| LattePanda Sigma (Sweetiebot) | ~2s for 30s audio | ~3s | Good |

VAD is negligible on all platforms (~2MB model, <10ms/frame).

## Implementation Plan

### Phase 1: Core Sidecar (2.0.6.0)

- [x] `projects/voice-sidecar/` Axum server, config, auth, limits, and cache
- [x] sherpa-rs/sherpa-onnx runtime integration
- [x] Silero VAD endpoint
- [x] Whisper STT endpoint
- [x] Kokoro TTS endpoint
- [x] Opt-in SHA-256-verified model installer
- [x] Separate health/ready/metrics listener
- [x] systemd + OpenRC service templates
- [x] Podman deployment docs

### Phase 2: Daemon Integration (2.0.6.0)

- [x] Existing transcription middleware integration through local provider
- [x] WASM tool `voice-transcribe` with host-scoped bearer injection
- [x] Registry-aware health check, mt-admin lifecycle, monitor-only self-heal
- [x] Per-tenant enable/disable and model configuration

### Phase 3: Advanced Features (2.0.8.0+)

- [ ] Streaming STT (real-time transcription via WebSocket)
- [ ] Speaker diarization (who said what)
- [ ] Voice response (agent text → audio → channel)
- [ ] Multiple STT/TTS engine selection per-tenant
- [ ] GPU acceleration (CUDA execution provider)

## Benefits

1. **Local voice processing** — no cloud, no API costs, no data leaving the machine
2. **Same architecture as LunarVision** — proven sidecar pattern, no new paradigms
3. **Configurable models** — match hardware capability (tiny on Pi5, large on dev VM)
4. **Privacy-first** — voice notes transcribed locally, never sent to third parties
5. **Accessibility** — agents can process audio messages, respond with speech
6. **Herd-wide** — every agent gains voice capability through the same sidecar

## Risks

- **sherpa-onnx C++ dependency** — build complexity (mitigated: pre-built binaries, container)
- **Model size** — 154MB default footprint (mitigated: int8 quantization, model selection)
- **CPU load during inference** — STT is compute-heavy (mitigated: rate limiting, caching, VAD pre-filter)
- **sherpa runtime churn** — native ABI/build changes (mitigated: pinned crate and container)
- **sherpa-onnx Rust bindings maturity** — docs.rs exists but API surface is large (mitigated: wrap behind clean interface)

## References

- LunarVision OCR sidecar: `projects/ocr-sidecar/` (reference implementation)
- Vision enrichment: `ic/src/vision_enrichment.rs` (daemon-side caller pattern)
- ort crate: https://github.com/pykeio/ort (v2.0.0-rc.13)
- sherpa-onnx: https://docs.rs/sherpa-onnx (full voice pipeline)
- transcribe-rs: https://github.com/cjpais/transcribe-rs (alternative STT)
- vox: https://github.com/mrtozner/vox (reference voice pipeline architecture)
- Silero VAD: github.com/snakers4/silero-vad
- Kokoro TTS: huggingface.co/hexgrad/Kokoro-82M
- Whisper ONNX: huggingface.co/openai/whisper-tiny

## Coaches/Reviewers

- Christopher — architecture, sidecar design, deployment strategy
- Kageho — security model (auth, network exposure, model provenance)
- Ruffles — coordination pipeline integration (voice commands over XMPP?)
- Volta — Pi5 resource feasibility testing
