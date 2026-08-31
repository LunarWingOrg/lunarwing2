use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;

use crate::audio::{AudioData, TARGET_SAMPLE_RATE};
use crate::config::Config;

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
            let recognizer = sherpa_rs::transducer::TransducerRecognizer::new(
                sherpa_rs::transducer::TransducerConfig {
                    encoder: display_path(&config.stt_encoder),
                    decoder: display_path(&config.stt_decoder),
                    joiner: display_path(&config.stt_joiner),
                    tokens: display_path(&config.stt_tokens),
                    num_threads: config.num_threads,
                    provider: Some("cpu".into()),
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
        if !language.is_empty()
            && language != "auto"
            && !self.language.is_empty()
            && language != self.language
        {
            return Err(EngineError::LanguageMismatch);
        }
        let fallback_language = self.language.clone();
        let permit = self
            .inference_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
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
        let permit = self
            .inference_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut tts = tts.lock().map_err(|_| EngineError::State)?;
            let audio = tts
                .create(&text, speaker_id, speed.clamp(0.5, 2.0))
                .map_err(|_| EngineError::Inference)?;
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
        let permit = self
            .inference_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
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

fn all_exist(paths: &[&PathBuf]) -> bool {
    paths.iter().all(|path| path.exists())
}

fn first_missing(paths: &[&PathBuf]) -> EngineError {
    let path = paths
        .iter()
        .find(|path| !path.exists())
        .map(|path| (*path).clone())
        .unwrap_or_default();
    EngineError::MissingModel(path)
}

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
