#[cfg(feature = "onnx")]
use std::collections::HashMap;
#[cfg(feature = "onnx")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use async_trait::async_trait;
use serde::Serialize;

use crate::audio::AudioData;
#[cfg(feature = "onnx")]
use crate::audio::TARGET_SAMPLE_RATE;
use crate::config::Config;
#[cfg(feature = "onnx")]
use crate::config::normalize_stt_language;

#[cfg(feature = "onnx")]
const STT_CHUNK_SECS: usize = 30;

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Transcription {
    pub text: String,
    pub language: String,
    pub confidence: Option<f32>,
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeechSegment {
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone)]
pub struct SynthesizedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineReadiness {
    pub ready: bool,
    pub stt_engine: Option<String>,
    pub tts_engine: Option<String>,
    pub vad_engine: Option<String>,
    pub models_loaded: Vec<String>,
}

/// Voice component sharing the sidecar's inference permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoiceComponent {
    Stt,
    Tts,
    Vad,
}

impl VoiceComponent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Tts => "tts",
            Self::Vad => "vad",
        }
    }
}

/// Per-component view of abandoned inference calls (WI#264).
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct ComponentAbandonment {
    /// Abandoned calls whose native inference has not returned yet.
    pub in_flight: usize,
    /// Total calls abandoned since process start.
    pub total: u64,
}

/// Abandonment accounting for all voice components.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct AbandonedInference {
    pub stt: ComponentAbandonment,
    pub tts: ComponentAbandonment,
    pub vad: ComponentAbandonment,
}

impl AbandonedInference {
    pub fn any_in_flight(&self) -> bool {
        self.stt.in_flight > 0 || self.tts.in_flight > 0 || self.vad.in_flight > 0
    }
}

/// Detects callers that stopped waiting on a blocking inference call.
///
/// Native ONNX inference (WI#264) cannot be safely interrupted once
/// `spawn_blocking` has entered it, so the sidecar never tries to cancel it.
/// Instead each in-flight call is tracked by a [`CallerGuard`] held by the
/// caller's future and a [`WorkerGuard`] held by the blocking closure. If the
/// caller's future is dropped while inference is still running (HTTP timeout
/// or client disconnect), the call is recorded as abandoned; new work for
/// that component is then rejected fast with [`EngineError::Busy`] until the
/// underlying inference returns. This bounds how long a zombie inference can
/// monopolize its component mutex and one of the shared permits, and keeps
/// unrelated components (e.g. STT while TTS is stuck) available.
#[derive(Debug, Default)]
pub struct InferenceTracker {
    abandoned: [AtomicU64; COMPONENT_COUNT],
    in_flight_abandoned: [AtomicUsize; COMPONENT_COUNT],
}

const COMPONENT_COUNT: usize = 3;

impl InferenceTracker {
    fn slot(component: VoiceComponent) -> usize {
        match component {
            VoiceComponent::Stt => 0,
            VoiceComponent::Tts => 1,
            VoiceComponent::Vad => 2,
        }
    }

    /// Begins tracking one inference call, returning both halves.
    ///
    /// The [`CallerGuard`] stays with the caller's async future — its drop on
    /// a timeout or client disconnect is the abandonment signal. The
    /// [`WorkerGuard`] is moved into the blocking closure and settles the
    /// accounting when the native inference returns.
    pub fn begin(self: &Arc<Self>, component: VoiceComponent) -> (CallerGuard, WorkerGuard) {
        let phase = Arc::new(Mutex::new(CallPhase::Active));
        (
            CallerGuard {
                tracker: Arc::clone(self),
                component,
                phase: Arc::clone(&phase),
            },
            WorkerGuard {
                tracker: Arc::clone(self),
                component,
                phase,
            },
        )
    }

    fn caller_left_while_running(&self, component: VoiceComponent) {
        let slot = Self::slot(component);
        self.abandoned[slot].fetch_add(1, Ordering::Relaxed);
        self.in_flight_abandoned[slot].fetch_add(1, Ordering::Relaxed);
    }

    fn worker_returned_after_abandonment(&self, component: VoiceComponent) {
        self.in_flight_abandoned[Self::slot(component)].fetch_sub(1, Ordering::Relaxed);
    }

    /// Current abandoned-inference counters, for health/metrics surfaces.
    pub fn snapshot(&self) -> AbandonedInference {
        let read = |slot: usize| ComponentAbandonment {
            in_flight: self.in_flight_abandoned[slot].load(Ordering::Relaxed),
            total: self.abandoned[slot].load(Ordering::Relaxed),
        };
        AbandonedInference {
            stt: read(0),
            tts: read(1),
            vad: read(2),
        }
    }

    fn in_flight(&self, component: VoiceComponent) -> usize {
        self.in_flight_abandoned[Self::slot(component)].load(Ordering::Relaxed)
    }

    /// Rejects new work for a component whose previous call was abandoned.
    ///
    /// The abandoned native inference still holds its permit and component
    /// mutex; failing fast here keeps callers from queueing behind it and
    /// lets the component recover the moment the underlying call returns.
    pub fn reject_if_abandoned(&self, component: VoiceComponent) -> Result<(), EngineError> {
        if self.in_flight(component) > 0 {
            Err(EngineError::Busy)
        } else {
            Ok(())
        }
    }
}

/// Phase of one tracked call, advanced exactly once per side (WI#264).
///
/// All transitions go through the per-call mutex shared by the guards, so a
/// phase change and its counter accounting are one atomic step: the worker
/// can never observe `CallerLeft` and decrement `in_flight_abandoned` before
/// the caller's increment is visible. A settled-flag swap alone cannot
/// guarantee that ordering — publishing `CALLER_LEFT` and incrementing the
/// counter were separate operations, so a descheduled caller let the worker
/// decrement a still-zero counter and wrap it to `usize::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CallPhase {
    #[default]
    Active,
    CallerLeft,
    WorkerReturned,
}

/// Locks a mutex, recovering from poisoning.
///
/// Abandonment accounting is advisory (health/admission), never a reason to
/// fail an inference call, so a panicked sibling thread's lock is taken over
/// rather than propagated.
fn lock_or_recover<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Half of the abandonment detector owned by the caller's async future.
///
/// Created by [`InferenceTracker::begin`] alongside its [`WorkerGuard`] pair.
/// When the caller's future is dropped while the call is still
/// [`CallPhase::Active`] (worker still running), the drop records the call as
/// abandoned; on normal completion the future's drop happens after the join
/// handle already observed `WorkerReturned`, so nothing is recorded.
#[derive(Debug)]
pub struct CallerGuard {
    tracker: Arc<InferenceTracker>,
    component: VoiceComponent,
    phase: Arc<Mutex<CallPhase>>,
}

impl Drop for CallerGuard {
    fn drop(&mut self) {
        let mut phase = lock_or_recover(&self.phase);
        if *phase == CallPhase::Active {
            *phase = CallPhase::CallerLeft;
            self.tracker.caller_left_while_running(self.component);
        }
    }
}

/// Half of the abandonment detector owned by the blocking closure.
///
/// Its drop advances the shared phase to `WorkerReturned` and, when the
/// caller half already recorded abandonment, clears the in-flight count so
/// the component is re-admitted. Because settlement happens in `Drop`, a
/// panic inside the blocking closure still re-admits the component.
#[derive(Debug)]
pub struct WorkerGuard {
    tracker: Arc<InferenceTracker>,
    component: VoiceComponent,
    phase: Arc<Mutex<CallPhase>>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let mut phase = lock_or_recover(&self.phase);
        let previous = *phase;
        *phase = CallPhase::WorkerReturned;
        if previous == CallPhase::CallerLeft {
            self.tracker
                .worker_returned_after_abandonment(self.component);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0} model is unavailable")]
    ModelUnavailable(&'static str),
    #[error("requested model does not match loaded model")]
    ModelMismatch,
    #[error("requested language does not match loaded Whisper language")]
    LanguageMismatch,
    #[error("unknown TTS voice")]
    UnknownVoice,
    #[error("voice inference failed")]
    Inference,
    #[error("voice engine state is unavailable")]
    State,
    #[error("voice inference capacity is busy")]
    Busy,
    #[error("required model file is missing: {0}")]
    MissingModel(PathBuf),
    #[error("failed to initialize {component}: {reason}")]
    Initialization {
        component: &'static str,
        reason: String,
    },
}

#[async_trait]
pub trait VoiceEngine: Send + Sync {
    async fn transcribe(
        &self,
        audio: AudioData,
        language: &str,
        model: &str,
    ) -> Result<Transcription, EngineError>;

    async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        speed: f32,
    ) -> Result<SynthesizedAudio, EngineError>;

    async fn detect(&self, audio: AudioData) -> Result<Vec<SpeechSegment>, EngineError>;

    /// Per-component abandoned-inference accounting (WI#264).
    ///
    /// Non-zero `in_flight` for a component means a caller timed out or
    /// disconnected while its native inference was running; new work for that
    /// component is rejected with [`EngineError::Busy`] until the underlying
    /// call returns.
    fn abandoned_inference(&self) -> AbandonedInference {
        AbandonedInference::default()
    }

    fn readiness(&self) -> EngineReadiness;
}

#[cfg(feature = "onnx")]
pub enum SttRecognizer {
    Whisper(sherpa_rs::whisper::WhisperRecognizer),
    Transducer(sherpa_rs::transducer::TransducerRecognizer),
}

#[cfg(feature = "onnx")]
pub struct SherpaVoiceEngine {
    stt: Option<Arc<Mutex<SttRecognizer>>>,
    vad: Option<Arc<Mutex<sherpa_rs::silero_vad::SileroVad>>>,
    tts: Option<Arc<Mutex<sherpa_rs::tts::KokoroTts>>>,
    model_name: String,
    language: String,
    voice_ids: HashMap<String, i32>,
    default_voice: String,
    inference_limit: Arc<tokio::sync::Semaphore>,
    tracker: Arc<InferenceTracker>,
}

#[cfg(feature = "onnx")]
impl SherpaVoiceEngine {
    pub fn load(config: &Config) -> Result<Self, EngineError> {
        let stt = load_stt(config)?;

        let vad = if config.enable_vad && config.vad_model.is_file() {
            let detector = sherpa_rs::silero_vad::SileroVad::new(
                sherpa_rs::silero_vad::SileroVadConfig {
                    model: display_path(&config.vad_model),
                    max_speech_duration: config.max_audio_secs as f32,
                    num_threads: Some(config.num_threads),
                    ..Default::default()
                },
                config.max_audio_secs as f32 + 5.0,
            )
            .map_err(|error| EngineError::Initialization {
                component: "VAD",
                reason: error.to_string(),
            })?;
            Some(Arc::new(Mutex::new(detector)))
        } else {
            None
        };

        let tts_paths = [
            &config.tts_model,
            &config.tts_voices,
            &config.tts_tokens,
            &config.tts_data_dir,
        ];
        let tts = if config.enable_tts && all_exist(&tts_paths) {
            let synthesizer = sherpa_rs::tts::KokoroTts::new(sherpa_rs::tts::KokoroTtsConfig {
                model: display_path(&config.tts_model),
                voices: display_path(&config.tts_voices),
                tokens: display_path(&config.tts_tokens),
                data_dir: display_path(&config.tts_data_dir),
                dict_dir: display_path(&config.tts_dict_dir),
                lexicon: config.tts_lexicon.clone(),
                length_scale: 1.0,
                onnx_config: sherpa_rs::OnnxConfig {
                    provider: "cpu".into(),
                    debug: false,
                    num_threads: config.num_threads,
                },
                ..Default::default()
            });
            Some(Arc::new(Mutex::new(synthesizer)))
        } else {
            None
        };

        Ok(Self {
            stt,
            vad,
            tts,
            model_name: config.model_name.clone(),
            language: config.stt_language.clone(),
            voice_ids: config.tts_voice_ids.clone(),
            default_voice: config.tts_default_voice.clone(),
            inference_limit: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent_inference)),
            tracker: Arc::new(InferenceTracker::default()),
        })
    }
}

#[cfg(feature = "onnx")]
fn load_stt(config: &Config) -> Result<Option<Arc<Mutex<SttRecognizer>>>, EngineError> {
    let mut required = vec![&config.stt_encoder, &config.stt_decoder, &config.stt_tokens];
    if config.stt_engine == crate::config::SttEngineKind::Parakeet {
        required.push(&config.stt_joiner);
    }
    if !all_exist(&required) {
        return if config.require_stt {
            Err(first_missing(&required))
        } else {
            Ok(None)
        };
    }
    let recognizer = match config.stt_engine {
        crate::config::SttEngineKind::Whisper => {
            let recognizer =
                sherpa_rs::whisper::WhisperRecognizer::new(sherpa_rs::whisper::WhisperConfig {
                    encoder: display_path(&config.stt_encoder),
                    decoder: display_path(&config.stt_decoder),
                    tokens: display_path(&config.stt_tokens),
                    language: config.stt_language.clone(),
                    provider: Some("cpu".into()),
                    num_threads: Some(config.num_threads),
                    ..Default::default()
                })
                .map_err(|error| EngineError::Initialization {
                    component: "STT",
                    reason: error.to_string(),
                })?;
            SttRecognizer::Whisper(recognizer)
        }
        crate::config::SttEngineKind::Parakeet => {
            // Parakeet NeMo-2.x exports (e.g. parakeet-tdt-0.6b-v2) must go
            // through sherpa-onnx's nemo_transducer impl: their decoders carry
            // NeMo-2.x input names (targets/states.1/...) instead of the
            // generic transducer x/h/c, and only that impl feeds them
            // correctly. The generic impl aborts mid-inference.
            let recognizer = sherpa_rs::transducer::TransducerRecognizer::new(
                sherpa_rs::transducer::TransducerConfig {
                    encoder: display_path(&config.stt_encoder),
                    decoder: display_path(&config.stt_decoder),
                    joiner: display_path(&config.stt_joiner),
                    tokens: display_path(&config.stt_tokens),
                    num_threads: config.num_threads,
                    provider: Some("cpu".into()),
                    model_type: String::from("nemo_transducer"),
                    ..Default::default()
                },
            )
            .map_err(|error| EngineError::Initialization {
                component: "STT",
                reason: error.to_string(),
            })?;
            SttRecognizer::Transducer(recognizer)
        }
    };
    Ok(Some(Arc::new(Mutex::new(recognizer))))
}

#[cfg(feature = "onnx")]
#[async_trait]
impl VoiceEngine for SherpaVoiceEngine {
    async fn transcribe(
        &self,
        audio: AudioData,
        language: &str,
        model: &str,
    ) -> Result<Transcription, EngineError> {
        let stt = self
            .stt
            .clone()
            .ok_or(EngineError::ModelUnavailable("STT"))?;
        if !model.is_empty() && model != self.model_name {
            return Err(EngineError::ModelMismatch);
        }
        let language =
            normalize_stt_language(language).map_err(|_| EngineError::LanguageMismatch)?;
        if !language.is_empty() && !self.language.is_empty() && language != self.language {
            return Err(EngineError::LanguageMismatch);
        }
        let fallback_language = self.language.clone();
        self.tracker.reject_if_abandoned(VoiceComponent::Stt)?;
        let permit = self
            .inference_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        let (_caller, worker) = self.tracker.begin(VoiceComponent::Stt);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _worker = worker;
            transcribe_chunks(&stt, &audio, &fallback_language)
        })
        .await
        .map_err(|_| EngineError::Inference)?
    }

    async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        speed: f32,
    ) -> Result<SynthesizedAudio, EngineError> {
        let tts = self
            .tts
            .clone()
            .ok_or(EngineError::ModelUnavailable("TTS"))?;
        let voice = if voice.is_empty() {
            &self.default_voice
        } else {
            voice
        };
        let speaker_id = self
            .voice_ids
            .get(voice)
            .copied()
            .or_else(|| voice.parse::<i32>().ok())
            .ok_or(EngineError::UnknownVoice)?;
        let text = text.to_string();
        self.tracker.reject_if_abandoned(VoiceComponent::Tts)?;
        let permit = self
            .inference_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        let (_caller, worker) = self.tracker.begin(VoiceComponent::Tts);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _worker = worker;
            let mut tts = tts.try_lock().map_err(|_| EngineError::Busy)?;
            let audio = tts
                .create(&text, speaker_id, speed.clamp(0.5, 2.0))
                .map_err(|_| EngineError::Inference)?;
            drop(tts);
            Ok(SynthesizedAudio {
                samples: audio.samples,
                sample_rate: audio.sample_rate,
            })
        })
        .await
        .map_err(|_| EngineError::Inference)?
    }

    async fn detect(&self, audio: AudioData) -> Result<Vec<SpeechSegment>, EngineError> {
        let vad = self
            .vad
            .clone()
            .ok_or(EngineError::ModelUnavailable("VAD"))?;
        self.tracker.reject_if_abandoned(VoiceComponent::Vad)?;
        let permit = self
            .inference_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        let (_caller, worker) = self.tracker.begin(VoiceComponent::Vad);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _worker = worker;
            detect_segments(&vad, &audio)
        })
        .await
        .map_err(|_| EngineError::Inference)?
    }

    fn readiness(&self) -> EngineReadiness {
        let mut models_loaded = Vec::new();
        if self.stt.is_some() {
            models_loaded.push("stt".to_string());
        }
        if self.vad.is_some() {
            models_loaded.push("vad".to_string());
        }
        if self.tts.is_some() {
            models_loaded.push("tts".to_string());
        }
        EngineReadiness {
            ready: self.stt.is_some(),
            stt_engine: self.stt.as_ref().map(|_| self.model_name.clone()),
            tts_engine: self.tts.as_ref().map(|_| "kokoro".to_string()),
            vad_engine: self.vad.as_ref().map(|_| "silero-v5".to_string()),
            models_loaded,
        }
    }

    fn abandoned_inference(&self) -> AbandonedInference {
        self.tracker.snapshot()
    }
}

#[cfg(feature = "onnx")]
fn transcribe_chunks(
    stt: &Mutex<SttRecognizer>,
    audio: &AudioData,
    fallback_language: &str,
) -> Result<Transcription, EngineError> {
    let mut recognizer = stt.lock().map_err(|_| EngineError::State)?;
    let chunk_samples = TARGET_SAMPLE_RATE as usize * STT_CHUNK_SECS;
    let mut segments = Vec::new();
    let mut language = fallback_language.to_string();
    for (index, chunk) in audio.samples.chunks(chunk_samples).enumerate() {
        // Whisper reports the detected language per chunk; parakeet-tdt
        // returns text only, so the configured language is kept.
        let text = match &mut *recognizer {
            SttRecognizer::Whisper(whisper) => {
                let result = whisper.transcribe(audio.sample_rate, chunk);
                if !result.lang.is_empty() {
                    language = result.lang;
                }
                result.text
            }
            SttRecognizer::Transducer(transducer) => {
                transducer.transcribe(audio.sample_rate, chunk)
            }
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let start_ms = index as u64 * STT_CHUNK_SECS as u64 * 1_000;
        let duration_ms = chunk.len() as u64 * 1_000 / u64::from(audio.sample_rate);
        segments.push(TranscriptSegment {
            start_ms,
            end_ms: start_ms + duration_ms,
            text,
        });
    }
    let text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Transcription {
        text,
        language,
        confidence: None,
        segments,
    })
}

#[cfg(feature = "onnx")]
fn detect_segments(
    vad: &Mutex<sherpa_rs::silero_vad::SileroVad>,
    audio: &AudioData,
) -> Result<Vec<SpeechSegment>, EngineError> {
    let mut vad = vad.lock().map_err(|_| EngineError::State)?;
    vad.clear();
    for window in audio.samples.chunks(512) {
        let mut samples = window.to_vec();
        samples.resize(512, 0.0);
        vad.accept_waveform(samples);
    }
    vad.flush();
    let mut segments = Vec::new();
    while !vad.is_empty() {
        let segment = vad.front();
        vad.pop();
        let start_ms = segment.start.max(0) as u64 * 1_000 / u64::from(audio.sample_rate);
        let duration_ms = segment.samples.len() as u64 * 1_000 / u64::from(audio.sample_rate);
        segments.push(SpeechSegment {
            start_ms,
            end_ms: start_ms + duration_ms,
        });
    }
    vad.clear();
    Ok(segments)
}

#[cfg(feature = "onnx")]
fn all_exist(paths: &[&PathBuf]) -> bool {
    paths.iter().all(|path| path.exists())
}

#[cfg(feature = "onnx")]
fn first_missing(paths: &[&PathBuf]) -> EngineError {
    let path = paths
        .iter()
        .find(|path| !path.exists())
        .map(|path| (*path).clone())
        .unwrap_or_default();
    EngineError::MissingModel(path)
}

#[cfg(feature = "onnx")]
fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(not(feature = "onnx"))]
pub struct SherpaVoiceEngine;

#[cfg(not(feature = "onnx"))]
impl SherpaVoiceEngine {
    pub fn load(_config: &Config) -> Result<Self, EngineError> {
        Err(EngineError::Initialization {
            component: "ONNX",
            reason: "binary built without the onnx feature".into(),
        })
    }
}

#[cfg(not(feature = "onnx"))]
#[async_trait]
impl VoiceEngine for SherpaVoiceEngine {
    async fn transcribe(
        &self,
        _audio: AudioData,
        _language: &str,
        _model: &str,
    ) -> Result<Transcription, EngineError> {
        Err(EngineError::ModelUnavailable("STT"))
    }

    async fn synthesize(
        &self,
        _text: &str,
        _voice: &str,
        _speed: f32,
    ) -> Result<SynthesizedAudio, EngineError> {
        Err(EngineError::ModelUnavailable("TTS"))
    }

    async fn detect(&self, _audio: AudioData) -> Result<Vec<SpeechSegment>, EngineError> {
        Err(EngineError::ModelUnavailable("VAD"))
    }

    fn readiness(&self) -> EngineReadiness {
        EngineReadiness {
            ready: false,
            stt_engine: None,
            tts_engine: None,
            vad_engine: None,
            models_loaded: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn normal_completion_records_no_abandonment() {
        let tracker = Arc::new(InferenceTracker::default());
        let (caller, worker) = tracker.begin(VoiceComponent::Tts);

        drop(worker);
        drop(caller);
        // Worker finished first: caller's drop sees WorkerReturned and does
        // nothing. Neither counter moved.
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.tts.total, 0);
        assert_eq!(snapshot.tts.in_flight, 0);
        assert!(tracker.reject_if_abandoned(VoiceComponent::Tts).is_ok());
    }

    #[test]
    fn abandonment_then_worker_return_re_admits_component() {
        let tracker = Arc::new(InferenceTracker::default());
        let (caller, worker) = tracker.begin(VoiceComponent::Tts);

        drop(caller);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.tts.total, 1);
        assert_eq!(snapshot.tts.in_flight, 1);
        assert!(matches!(
            tracker.reject_if_abandoned(VoiceComponent::Tts),
            Err(EngineError::Busy)
        ));

        drop(worker);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.tts.total, 1);
        assert_eq!(snapshot.tts.in_flight, 0);
        assert!(tracker.reject_if_abandoned(VoiceComponent::Tts).is_ok());
    }

    #[test]
    fn worker_panic_still_re_admits_component() {
        let tracker = Arc::new(InferenceTracker::default());
        let (caller, worker) = tracker.begin(VoiceComponent::Stt);

        drop(caller);
        // A panic inside the blocking closure unwinds through WorkerGuard's
        // drop, which must still settle the accounting.
        let caught = panic::catch_unwind(AssertUnwindSafe(move || {
            let _held = worker;
            panic!("native inference panicked");
        }));
        assert!(caught.is_err());
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.stt.total, 1);
        assert_eq!(snapshot.stt.in_flight, 0);
        assert!(tracker.reject_if_abandoned(VoiceComponent::Stt).is_ok());
    }

    /// Concurrent caller-drop/worker-return races must never underflow the
    /// in-flight counter (WI#264 review P2): the worker cannot decrement
    /// before the abandonment increment is visible because both go through
    /// the shared per-call phase lock.
    #[test]
    fn concurrent_guard_drops_never_underflow_in_flight_counter() {
        const DROPS: usize = 64;
        for _ in 0..DROPS {
            let tracker = Arc::new(InferenceTracker::default());
            let (caller, worker) = tracker.begin(VoiceComponent::Tts);
            let barrier = Arc::new(Barrier::new(2));
            let caller_barrier = Arc::clone(&barrier);
            let worker_barrier = barrier;

            let caller_thread = thread::spawn(move || {
                caller_barrier.wait();
                drop(caller);
            });
            let worker_thread = thread::spawn(move || {
                worker_barrier.wait();
                drop(worker);
            });
            caller_thread.join().expect("caller thread panicked");
            worker_thread.join().expect("worker thread panicked");

            let snapshot = tracker.snapshot();
            // Either drop order is a valid settled state: caller-first
            // records total=1 then settles in_flight to 0; worker-first
            // settles first and the caller's drop records nothing. What is
            // never valid is the wrap-to-usize::MAX failure mode from the
            // rejected MR, where the worker decremented before the caller's
            // increment was visible.
            assert!(snapshot.tts.total <= 1);
            assert_eq!(snapshot.tts.in_flight, 0);
        }
    }
}
