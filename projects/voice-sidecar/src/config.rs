use std::collections::HashMap;
use std::env;
use std::net::IpAddr;
use std::path::PathBuf;

use secrecy::SecretString;

const DEFAULT_MODEL_DIR: &str = "/models";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("VOICE_AUTH_TOKEN is required unless VOICE_ALLOW_UNAUTHENTICATED=true")]
    MissingAuthToken,
    #[error("invalid boolean for {0}")]
    InvalidBool(&'static str),
    #[error("invalid integer for {0}")]
    InvalidInteger(&'static str),
    #[error("VOICE_BIND_HOST must be an IP address")]
    InvalidBindHost,
    #[error("VOICE_PORT and VOICE_HEALTH_PORT must be different")]
    PortConflict,
    #[error("VOICE_TTS_VOICES_JSON must be a JSON object mapping names to integer speaker IDs")]
    InvalidVoiceMap,
}

pub struct Config {
    pub bind_host: IpAddr,
    pub port: u16,
    pub health_port: u16,
    pub auth_token: Option<SecretString>,
    pub max_audio_bytes: usize,
    pub max_audio_secs: u64,
    pub max_text_chars: usize,
    pub max_tts_audio_secs: u64,
    pub request_timeout_secs: u64,
    pub max_concurrent_inference: usize,
    pub rate_limit_per_second: u32,
    pub cache_ttl_secs: u64,
    pub cache_max_entries: usize,
    pub ffmpeg_path: PathBuf,
    pub model_name: String,
    pub stt_encoder: PathBuf,
    pub stt_decoder: PathBuf,
    pub stt_tokens: PathBuf,
    pub stt_language: String,
    pub vad_model: PathBuf,
    pub tts_model: PathBuf,
    pub tts_voices: PathBuf,
    pub tts_tokens: PathBuf,
    pub tts_data_dir: PathBuf,
    pub tts_dict_dir: PathBuf,
    pub tts_lexicon: String,
    pub tts_default_voice: String,
    pub tts_voice_ids: HashMap<String, i32>,
    pub enable_vad: bool,
    pub enable_tts: bool,
    pub require_stt: bool,
    pub num_threads: i32,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let model_dir = env_path("VOICE_MODEL_DIR", DEFAULT_MODEL_DIR);
        let allow_unauthenticated = env_bool("VOICE_ALLOW_UNAUTHENTICATED", false)?;
        let auth_token = env::var("VOICE_AUTH_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .map(SecretString::from);
        if auth_token.is_none() && !allow_unauthenticated {
            return Err(ConfigError::MissingAuthToken);
        }

        let bind_host = env_string("VOICE_BIND_HOST", "127.0.0.1")
            .parse()
            .map_err(|_| ConfigError::InvalidBindHost)?;
        let port = env_number("VOICE_PORT", 8090u16)?;
        let health_port = env_number("VOICE_HEALTH_PORT", 8091u16)?;
        if port == health_port {
            return Err(ConfigError::PortConflict);
        }

        let tts_voice_ids = parse_voice_map()?;
        Ok(Self {
            bind_host,
            port,
            health_port,
            auth_token,
            max_audio_bytes: env_number("VOICE_MAX_AUDIO_BYTES", 50 * 1024 * 1024usize)?,
            max_audio_secs: env_number("VOICE_MAX_AUDIO_SECS", 300u64)?,
            max_text_chars: env_number("VOICE_MAX_TEXT_CHARS", 10_000usize)?,
            max_tts_audio_secs: env_number("VOICE_MAX_TTS_AUDIO_SECS", 300u64)?,
            request_timeout_secs: env_number("VOICE_REQUEST_TIMEOUT_SECS", 120u64)?,
            max_concurrent_inference: env_number("VOICE_MAX_CONCURRENT_INFERENCE", 2usize)?.max(1),
            rate_limit_per_second: env_number("VOICE_RATE_LIMIT_PER_SEC", 5u32)?.max(1),
            cache_ttl_secs: env_number("VOICE_CACHE_TTL_SECS", 600u64)?,
            cache_max_entries: env_number("VOICE_CACHE_MAX_ENTRIES", 5_000usize)?.max(1),
            ffmpeg_path: env_path("VOICE_FFMPEG_PATH", "ffmpeg"),
            model_name: env_string("VOICE_STT_MODEL", "whisper-tiny"),
            stt_encoder: env_path_or_join(
                "VOICE_STT_ENCODER",
                &model_dir,
                "sherpa-onnx-whisper-tiny/tiny-encoder.int8.onnx",
            ),
            stt_decoder: env_path_or_join(
                "VOICE_STT_DECODER",
                &model_dir,
                "sherpa-onnx-whisper-tiny/tiny-decoder.int8.onnx",
            ),
            stt_tokens: env_path_or_join(
                "VOICE_STT_TOKENS",
                &model_dir,
                "sherpa-onnx-whisper-tiny/tiny-tokens.txt",
            ),
            stt_language: env_string("VOICE_STT_LANGUAGE", "auto"),
            vad_model: env_path_or_join("VOICE_VAD_MODEL", &model_dir, "silero_vad.onnx"),
            tts_model: env_path_or_join(
                "VOICE_TTS_MODEL",
                &model_dir,
                "kokoro-multi-lang-v1_0/model.onnx",
            ),
            tts_voices: env_path_or_join(
                "VOICE_TTS_VOICES",
                &model_dir,
                "kokoro-multi-lang-v1_0/voices.bin",
            ),
            tts_tokens: env_path_or_join(
                "VOICE_TTS_TOKENS",
                &model_dir,
                "kokoro-multi-lang-v1_0/tokens.txt",
            ),
            tts_data_dir: env_path_or_join(
                "VOICE_TTS_DATA_DIR",
                &model_dir,
                "kokoro-multi-lang-v1_0/espeak-ng-data",
            ),
            tts_dict_dir: env_path_or_join(
                "VOICE_TTS_DICT_DIR",
                &model_dir,
                "kokoro-multi-lang-v1_0/dict",
            ),
            tts_lexicon: env::var("VOICE_TTS_LEXICON").unwrap_or_else(|_| {
                format!(
                    "{},{}",
                    model_dir
                        .join("kokoro-multi-lang-v1_0/lexicon-us-en.txt")
                        .display(),
                    model_dir
                        .join("kokoro-multi-lang-v1_0/lexicon-zh.txt")
                        .display()
                )
            }),
            tts_default_voice: env_string("VOICE_TTS_DEFAULT_VOICE", "af_heart"),
            tts_voice_ids,
            enable_vad: env_bool("VOICE_ENABLE_VAD", true)?,
            enable_tts: env_bool("VOICE_ENABLE_TTS", true)?,
            require_stt: env_bool("VOICE_REQUIRE_STT", true)?,
            num_threads: env_number("ORT_NUM_THREADS", 4i32)?.max(1),
        })
    }

    pub fn max_body_bytes(&self) -> usize {
        self.max_audio_bytes
            .saturating_mul(4)
            .saturating_div(3)
            .saturating_add(128 * 1024)
    }
}

fn env_string(key: &'static str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_path(key: &'static str, default: &str) -> PathBuf {
    env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| default.into())
}

fn env_path_or_join(key: &'static str, root: &std::path::Path, suffix: &str) -> PathBuf {
    env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(suffix))
}

fn env_bool(key: &'static str, default: bool) -> Result<bool, ConfigError> {
    match env::var(key) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(ConfigError::InvalidBool(key)),
        },
        Err(_) => Ok(default),
    }
}

fn env_number<T>(key: &'static str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    match env::var(key) {
        Ok(value) => value.parse().map_err(|_| ConfigError::InvalidInteger(key)),
        Err(_) => Ok(default),
    }
}

fn parse_voice_map() -> Result<HashMap<String, i32>, ConfigError> {
    let default = HashMap::from([("af_heart".to_string(), 0)]);
    let Ok(value) = env::var("VOICE_TTS_VOICES_JSON") else {
        return Ok(default);
    };
    serde_json::from_str(&value).map_err(|_| ConfigError::InvalidVoiceMap)
}
