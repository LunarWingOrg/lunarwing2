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
- Inference admission, caller wait time, and returned synthesized duration are
  bounded. Native inference itself cannot be interrupted safely; see recovery below.
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
GET /ready    STT readiness and inference state; 503 while draining or failed
GET /metrics  bounded operational counters
```

### Inference Timeout, Admission, and Recovery (WI#264)

`VOICE_REQUEST_TIMEOUT_SECS` bounds the HTTP wait, not the native ONNX call.
A timeout returns **504** (`timeout`); dropping the request future also
abandons its wait. Neither can safely interrupt `KokoroTts::create` or other
already-running native inference. A disconnected transport may keep the
handler alive until its timeout; this does not bypass admission limits.

All three components share one inference runner. It admits **at most one call
per component before acquiring a shared permit**, then moves both permits into
the blocking worker. Requests for an occupied component return **503**
(`unavailable`) immediately, even before the original request times out. They
cannot queue behind its model mutex or take additional shared permits. A
timed-out TTS call can therefore retain only one permit. With the default two
permits, STT/VAD can use the remaining capacity; with a limit of one, or other
components also occupying the pool, they return 503 too. This is admission
isolation, not native cancellation or a guarantee of inference completion.

When abandoned inference returns normally (including a returned inference
error), its mutex/permits are released and admission recovers automatically.
Caller/worker settlement is serialized so a concurrent completion cannot
underflow abandonment counts. An ordinary processing failure remains **422**
(`processing_failed`).

**Panics require restart.** A Rust panic while holding a model mutex releases
permits through unwinding but may leave native state invalid. The component is
marked failed and later calls return **503** (`component_failed`, restart
required), not transient Busy. A pre-poisoned model lock is handled the same
way. No poisoned model is reused or silently cleared. Other components remain
admissible if capacity is available. A native process abort cannot be
recovered in-process either.

`/ready` and `/metrics` expose an additive `inference` object for `stt`,
`tts`, and `vad`. Each reports `active`, `abandoned_in_flight` (0 or 1),
`abandoned_total` (since process start), and `failed`. `/ready` is 503
whenever STT is absent, any abandoned inference is still draining, or any
component is failed; normal busy work alone does not make it unready.
`/health` remains process liveness (200). A returning worker clears current
abandonment, not its cumulative total or permanent failure state.

If a call **never returns**, admission stays bounded but that component
remains unavailable and retains one shared permit. Inspect `/ready` and
`/metrics` on the tenant's registry `voice_health` port (not hardcoded
standalone ports). Persistent abandonment or `failed: true` requires
restarting the process. For a managed tenant use the supported
init-agnostic lifecycle command:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
```

Use the installed/configured mt-admin path if different. **This restarts the
whole tenant, including the voice sidecar**, and interrupts its other
workloads; schedule recovery accordingly. Recheck both `/health` and `/ready`
afterward. Do not use bare tenant-unit `systemctl`/`rc-service` commands.
LunarVoice's health monitor alerts but does not automatically restart model
workloads. Raising the timeout or shared permit count is not a repair for a
hung or poisoned model. The gateway retains WI#259's 422/503/504 mappings;
its 503 response is generic, so operators use the sidecar's operational
endpoint to distinguish failure from temporary contention.

## Multi-Tenant Lifecycle

Use `ic/scripts/lunarwing-mt-admin.sh`; do not hand-create tenant units or
containers. Lifecycle mutations use the tenant lock, work on both systemd user
units and OpenRC, and leave the feature disabled until explicit activation.

1. Copy `model-manifest.example.json` and replace required placeholders with
   exact SHA-256 values obtained through a trusted release channel.
2. Install the shared image, tenant configuration, and optionally the verified
   models. Installation does not start the sidecar or alter transcription:

   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh install-voice <tenant> \
     --model-manifest /secure/path/model-manifest.json
   ```

3. Configure the inactive service, for example:

   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh configure-voice <tenant> \
     --stt-engine whisper --stt-model whisper-tiny \
     --stt-language auto --enable-vad --enable-tts \
     --max-audio-secs 300 --max-concurrent-inference 2
   ```

   Run `lunarwing-mt-admin.sh help` for every managed flag. Active
   reconfiguration recreates/restarts the sidecar when needed, verifies
   `/health` and `/ready`, restarts the tenant daemon when it is running, and
   restores the prior files/runtime if reconciliation fails.

4. Activate and verify:

   ```bash
   sudo ic/scripts/lunarwing-mt-admin.sh activate-voice <tenant>
   sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
   sudo env SELF_HEAL_TENANTS_FILE=/etc/lunarwing/ports.json \
     ic-infrastructure-health-check/health-lunarvoice.sh
   ```

   Activation requires a matching SHA marker and readable, non-symlinked STT
   artifacts below the shared model root. It selects the local transcription
   provider only after those checks, starts through the detected init system,
   and rolls back the provider and runtime on failure.

`deactivate-voice` stops and deregisters the sidecar and restores the
transcription enabled/provider/model values that were active before
LunarVoice. `remove-voice` additionally removes the tenant container, unit,
`voice.env`, and daemon-side voice URL/token. Shared model files and the shared
image are retained deliberately. `enable-voice` and `disable-voice` remain
compatibility aliases for activate/deactivate.

## Models

LunarVoice expects sherpa-onnx model layouts. For a Whisper model named
`whisper-<size>`, derived paths are:

```text
/models/sherpa-onnx-whisper-<size>/
  <size>-encoder.int8.onnx
  <size>-decoder.int8.onnx
  <size>-tokens.txt
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

`VOICE_STT_ENGINE` selects the speech-to-text engine:

- `whisper` (default) is multilingual and reports the detected language per
  chunk. The default model is `whisper-tiny`. `auto` (or an empty value) is
  normalized to sherpa-onnx's auto-detection value; explicit `VOICE_STT_LANGUAGE`
  values must be supported Whisper ISO codes such as `en` or `ja`.
- `parakeet` uses NVIDIA Parakeet-TDT through the sherpa-onnx transducer
  recognizer. The supplied manifest template includes
  `parakeet-tdt-0.6b-v2-int8` (English) and
  `parakeet-tdt-0.6b-v3-int8` (multilingual). Its layout is:

```text
/models/sherpa-onnx-nemo-<VOICE_STT_MODEL>/
  encoder.int8.onnx
  decoder.int8.onnx
  joiner.int8.onnx
  tokens.txt
```

Parakeet does not detect language; its response uses
`VOICE_STT_LANGUAGE`, which should be explicit for a monolingual model.
Explicit STT path variables override the derived locations.

Models are never committed or downloaded implicitly by mt-admin. The installer
requires HTTPS, exact SHA-256 values, safe archive members, and an absent or
previously verified destination. It serializes concurrent installs and writes
`.lunarvoice-<model>.sha256` markers used by activation. Container-only
`VOICE_AUTO_DOWNLOAD=true` uses the same verifier, but explicit
`install-voice --model-manifest` is the production path.

## Configuration Surface

`.env.example` is a copyable template. Numeric values outside these inclusive
ranges fail startup instead of being silently clamped.

| Variable | Default / range | Purpose |
|---|---|---|
| `VOICE_AUTH_TOKEN` | required | Bearer token for all three API endpoints |
| `VOICE_ALLOW_UNAUTHENTICATED` | `false` | Development escape hatch; never use on a network-reachable listener |
| `VOICE_BIND_HOST` | `127.0.0.1` | IP address for both listeners; containers bind internally to `0.0.0.0` and publish loopback only |
| `VOICE_PORT` | `8090`, `1..65535` | Authenticated API listener |
| `VOICE_HEALTH_PORT` | `8091`, `1..65535` | Unauthenticated operational listener; must differ from `VOICE_PORT` |
| `VOICE_MODEL_DIR` | `/models` | Root for derived model paths |
| `VOICE_STT_ENGINE` | `whisper` | `whisper` or `parakeet` (`parakeet-tdt`/`transducer` are accepted direct-env aliases) |
| `VOICE_STT_MODEL` | `whisper-tiny` | Loaded model name and derived artifact directory |
| `VOICE_STT_LANGUAGE` | `auto` | Whisper request constraint or Parakeet response language |
| `VOICE_STT_ENCODER` | derived | Absolute encoder override |
| `VOICE_STT_DECODER` | derived | Absolute decoder override |
| `VOICE_STT_JOINER` | derived | Parakeet joiner override |
| `VOICE_STT_TOKENS` | derived | Token-file override |
| `VOICE_REQUIRE_STT` | `true` | Fail engine startup when STT is absent; production readiness still requires STT |
| `VOICE_ENABLE_VAD` | `true` | Load Silero VAD when its model exists |
| `VOICE_VAD_MODEL` | `/models/silero_vad.onnx` | Silero model path |
| `VOICE_ENABLE_TTS` | `true` | Load Kokoro TTS when all required artifacts exist |
| `VOICE_TTS_MODEL` | derived | Kokoro ONNX model path |
| `VOICE_TTS_VOICES` | derived | Kokoro speaker embedding file |
| `VOICE_TTS_TOKENS` | derived | Kokoro token file |
| `VOICE_TTS_DATA_DIR` | derived | espeak-ng data directory |
| `VOICE_TTS_DICT_DIR` | derived | Kokoro dictionary directory |
| `VOICE_TTS_LEXICON` | two derived lexicons | Comma-separated lexicon paths |
| `VOICE_TTS_DEFAULT_VOICE` | `af_heart` | Voice used when a request omits `voice` |
| `VOICE_TTS_VOICES_JSON` | `{"af_heart":0}` | JSON object mapping voice names to integer speaker IDs |
| `VOICE_MAX_AUDIO_BYTES` | `52428800`, `1024..1073741824` | Maximum decoded upload bytes |
| `VOICE_MAX_AUDIO_SECS` | `300`, `1..3600` | Maximum decoded input duration |
| `VOICE_MAX_TEXT_CHARS` | `10000`, `1..100000` | Maximum TTS Unicode scalar count |
| `VOICE_MAX_TTS_AUDIO_SECS` | `300`, `1..3600` | Maximum synthesized duration |
| `VOICE_REQUEST_TIMEOUT_SECS` | `120`, `1..3600` | Decode/inference caller timeout; does not terminate native inference |
| `VOICE_MAX_CONCURRENT_INFERENCE` | `2`, `1..64` | Shared STT/TTS/VAD permits, additionally limited to one admitted call per component |
| `VOICE_RATE_LIMIT_PER_SEC` | `5`, `1..10000` | Process-wide API token-bucket rate |
| `VOICE_CACHE_TTL_SECS` | `600`, `0..86400` | Transcription cache TTL; zero disables cache hits |
| `VOICE_CACHE_MAX_ENTRIES` | `5000`, `1..1000000` | Cache entry bound |
| `VOICE_FFMPEG_PATH` | `ffmpeg` | Executable used directly for non-WAV input |
| `ORT_NUM_THREADS` | `4`, `1..128` | Threads passed to each ONNX component |
| `VOICE_AUTO_DOWNLOAD` | `false` | Container entrypoint model install switch |
| `VOICE_MODEL_MANIFEST` | `/models/model-manifest.json` | Verified installer manifest path |
| `RUST_LOG` | `lunarwing_voice_sidecar=info` | `tracing` filter |

Mt-admin owns `VOICE_ENABLED`, forces authentication, internal container
addresses/ports, explicit model installation, and required STT. Its
`configure-voice` command exposes the common model, feature, limit, cache,
voice, and thread settings; advanced artifact paths and voice maps are direct
`voice.env` settings for operators who manage custom model layouts.

Daemon integration uses `TRANSCRIPTION_ENABLED`,
`TRANSCRIPTION_PROVIDER=lunarvoice`, `TRANSCRIPTION_MODEL`, the loopback-only
`VOICE_SERVICE_URL`, and the dedicated `VOICE_AUTH_TOKEN`. Mt-admin writes those
values and preserves/restores a pre-existing cloud transcription selection.

## Health And Operations

`/health` is process liveness; `/ready` returns HTTP 200 only when STT is
loaded and no component is draining abandoned inference or failed (see
[Inference Timeout, Admission, and Recovery](#inference-timeout-admission-and-recovery-wi264));
`/metrics` exposes bounded
request, cache, rate-limit, error, and per-component inference counters.
The systemd/OpenRC and mt-admin startup gates require both liveness and
readiness; Docker and Compose mark the container healthy only after both pass.
`health-lunarvoice.sh` discovers enabled tenants
from `voice_health` registry slots, probes all of them, and fails closed on
malformed enablement, missing/duplicate ports, unreachable instances, or
unready inference. LunarVoice remains monitor-only in self-heal: failures alert
but do not automatically restart optional model workloads.

## Build And Test

Run from `projects/voice-sidecar/`. Use repository resource limits; put longer
all-feature runs in tmux:

```bash
taskset -c 0-11 cargo fmt --all -- --check
# Model-free tests include the production inference runner with controlled
# blocking work, shared permits, cancellation, and a real poisoned mutex.
taskset -c 0-11 cargo test --locked -j12 --no-default-features -- --test-threads=12

# Native-feature checks use the committed, hash-verified libraries; no model download.
(cd native-libs && sha256sum -c SHA256SUMS)
export SHERPA_LIB_PATH="$PWD/native-libs/linux-x64"
export LD_LIBRARY_PATH="$SHERPA_LIB_PATH/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
taskset -c 0-11 cargo check --locked -j12 --all-targets
taskset -c 0-11 cargo test --locked -j12 -- --test-threads=12
taskset -c 0-11 cargo clippy --locked -j12 --all-targets -- -D warnings

# Repository-root lifecycle and health harnesses:
bash ic/scripts/tests/test-mt-admin-voice.sh
bash ic-infrastructure-health-check/tests/test-health-lunarvoice.sh
bash ic-infrastructure-health-check/tests/test-lunarvoice-init-templates.sh
```

See `PODMAN_DEPLOYMENT.md` for standalone deployment details.
