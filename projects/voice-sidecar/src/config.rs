use std::collections::HashMap;
use std::env;
use std::net::IpAddr;
use std::path::PathBuf;

use secrecy::SecretString;

const DEFAULT_MODEL_DIR: &str = "/models";

const WHISPER_LANGUAGE_CODES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su",
];

pub(crate) fn normalize_stt_language(value: &str) -> Result<String, ConfigError> {
    let language = value.trim().to_ascii_lowercase();
    if language.is_empty() || language == "auto" {
        return Ok(String::new());
    }
    if WHISPER_LANGUAGE_CODES.contains(&language.as_str()) {
        Ok(language)
    } else {
        Err(ConfigError::InvalidSttLanguage)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("VOICE_AUTH_TOKEN is required unless VOICE_ALLOW_UNAUTHENTICATED=true")]
    MissingAuthToken,
    #[error("invalid boolean for {0}")]
    InvalidBool(&'static str),
    #[error("invalid integer for {0}")]
    InvalidInteger(&'static str),
    #[error("value for {0} is outside the supported range")]
    OutOfRange(&'static str),
    #[error("VOICE_BIND_HOST must be an IP address")]
    InvalidBindHost,
    #[error("VOICE_PORT and VOICE_HEALTH_PORT must be different non-zero ports")]
    PortConflict,
    #[error("VOICE_TTS_VOICES_JSON must be a JSON object mapping names to integer speaker IDs")]
    InvalidVoiceMap,
    #[error("VOICE_STT_ENGINE must be one of: whisper, parakeet")]
    InvalidSttEngine,
    #[error("VOICE_STT_MODEL must be a safe model name compatible with VOICE_STT_ENGINE")]
    InvalidSttModel,
    #[error("VOICE_STT_LANGUAGE must be auto or a supported Whisper language code")]
    InvalidSttLanguage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttEngineKind {
    Whisper,
    Parakeet,
}

impl SttEngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::Parakeet => "parakeet",
        }
    }
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
    pub stt_engine: SttEngineKind,
    pub stt_encoder: PathBuf,
    pub stt_decoder: PathBuf,
    pub stt_joiner: PathBuf,
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

struct RuntimeLimits {
    max_audio_bytes: usize,
    max_audio_secs: u64,
    max_text_chars: usize,
    max_tts_audio_secs: u64,
    request_timeout_secs: u64,
    max_concurrent_inference: usize,
    rate_limit_per_second: u32,
    cache_ttl_secs: u64,
    cache_max_entries: usize,
    num_threads: i32,
}

impl RuntimeLimits {
    fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            max_audio_bytes: env_number_in_range(
                "VOICE_MAX_AUDIO_BYTES",
                50 * 1024 * 1024usize,
                1024,
                1024 * 1024 * 1024,
            )?,
            max_audio_secs: env_number_in_range("VOICE_MAX_AUDIO_SECS", 300u64, 1, 3_600)?,
            max_text_chars: env_number_in_range("VOICE_MAX_TEXT_CHARS", 10_000usize, 1, 100_000)?,
            max_tts_audio_secs: env_number_in_range("VOICE_MAX_TTS_AUDIO_SECS", 300u64, 1, 3_600)?,
            request_timeout_secs: env_number_in_range(
                "VOICE_REQUEST_TIMEOUT_SECS",
                120u64,
                1,
                3_600,
            )?,
            max_concurrent_inference: env_number_in_range(
                "VOICE_MAX_CONCURRENT_INFERENCE",
                2usize,
                1,
                64,
            )?,
            rate_limit_per_second: env_number_in_range(
                "VOICE_RATE_LIMIT_PER_SEC",
                5u32,
                1,
                10_000,
            )?,
            cache_ttl_secs: env_number_in_range("VOICE_CACHE_TTL_SECS", 600u64, 0, 86_400)?,
            cache_max_entries: env_number_in_range(
                "VOICE_CACHE_MAX_ENTRIES",
                5_000usize,
                1,
                1_000_000,
            )?,
            num_threads: env_number_in_range("ORT_NUM_THREADS", 4i32, 1, 128)?,
        })
    }
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
        let port = env_number_in_range("VOICE_PORT", 8090u16, 1, u16::MAX)?;
        let health_port = env_number_in_range("VOICE_HEALTH_PORT", 8091u16, 1, u16::MAX)?;
        if port == health_port {
            return Err(ConfigError::PortConflict);
        }

        let limits = RuntimeLimits::from_env()?;
        let tts_voice_ids = parse_voice_map()?;
        let model_name = env_string("VOICE_STT_MODEL", "whisper-tiny");
        let stt_engine = env_stt_engine()?;
        let stt_paths = SttPathDefaults::new(&model_name, stt_engine)?;
        let stt_language = normalize_stt_language(&env_string("VOICE_STT_LANGUAGE", "auto"))?;
        Ok(Self {
            bind_host,
            port,
            health_port,
            auth_token,
            max_audio_bytes: limits.max_audio_bytes,
            max_audio_secs: limits.max_audio_secs,
            max_text_chars: limits.max_text_chars,
            max_tts_audio_secs: limits.max_tts_audio_secs,
            request_timeout_secs: limits.request_timeout_secs,
            max_concurrent_inference: limits.max_concurrent_inference,
            rate_limit_per_second: limits.rate_limit_per_second,
            cache_ttl_secs: limits.cache_ttl_secs,
            cache_max_entries: limits.cache_max_entries,
            ffmpeg_path: env_path("VOICE_FFMPEG_PATH", "ffmpeg"),
            model_name,
            stt_engine,
            stt_encoder: env_path_or_join("VOICE_STT_ENCODER", &model_dir, &stt_paths.encoder),
            stt_decoder: env_path_or_join("VOICE_STT_DECODER", &model_dir, &stt_paths.decoder),
            stt_joiner: env_path_or_join("VOICE_STT_JOINER", &model_dir, &stt_paths.joiner),
            stt_tokens: env_path_or_join("VOICE_STT_TOKENS", &model_dir, &stt_paths.tokens),
            stt_language,
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
            num_threads: limits.num_threads,
        })
    }

    pub fn max_body_bytes(&self) -> usize {
        self.max_audio_bytes
            .saturating_mul(4)
            .saturating_div(3)
            .saturating_add(128 * 1024)
    }

    pub fn max_text_body_bytes(&self) -> usize {
        // A JSON `\uXXXX` pair can encode one Unicode scalar in 12 bytes.
        self.max_text_chars
            .saturating_mul(12)
            .saturating_add(64 * 1024)
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

fn env_number_in_range<T>(
    key: &'static str,
    default: T,
    minimum: T,
    maximum: T,
) -> Result<T, ConfigError>
where
    T: std::str::FromStr + PartialOrd + Copy,
{
    let value = env_number(key, default)?;
    if value < minimum || value > maximum {
        return Err(ConfigError::OutOfRange(key));
    }
    Ok(value)
}

fn env_stt_engine() -> Result<SttEngineKind, ConfigError> {
    match env::var("VOICE_STT_ENGINE") {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "whisper" => Ok(SttEngineKind::Whisper),
            "parakeet" | "parakeet-tdt" | "transducer" => Ok(SttEngineKind::Parakeet),
            _ => Err(ConfigError::InvalidSttEngine),
        },
        Err(_) => Ok(SttEngineKind::Whisper),
    }
}

/// Model-relative default artifact paths for the selected STT engine.
/// Parakeet archives ship as `sherpa-onnx-nemo-<model>/` with uniform file
/// names; Whisper keeps its historical per-size names and has no joiner.
struct SttPathDefaults {
    encoder: String,
    decoder: String,
    joiner: String,
    tokens: String,
}

impl SttPathDefaults {
    fn new(model_name: &str, engine: SttEngineKind) -> Result<Self, ConfigError> {
        if !valid_model_name(model_name) {
            return Err(ConfigError::InvalidSttModel);
        }
        match engine {
            SttEngineKind::Whisper => {
                let size = model_name
                    .strip_prefix("whisper-")
                    .filter(|size| !size.is_empty())
                    .ok_or(ConfigError::InvalidSttModel)?;
                let root = format!("sherpa-onnx-{model_name}");
                Ok(Self {
                    encoder: format!("{root}/{size}-encoder.int8.onnx"),
                    decoder: format!("{root}/{size}-decoder.int8.onnx"),
                    joiner: format!("{root}/{size}-tokens.txt"),
                    tokens: format!("{root}/{size}-tokens.txt"),
                })
            }
            SttEngineKind::Parakeet => {
                let root = format!("sherpa-onnx-nemo-{model_name}");
                Ok(Self {
                    encoder: format!("{root}/encoder.int8.onnx"),
                    decoder: format!("{root}/decoder.int8.onnx"),
                    joiner: format!("{root}/joiner.int8.onnx"),
                    tokens: format!("{root}/tokens.txt"),
                })
            }
        }
    }
}

fn valid_model_name(model_name: &str) -> bool {
    !model_name.is_empty()
        && model_name.len() <= 128
        && model_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn parse_voice_map() -> Result<HashMap<String, i32>, ConfigError> {
    let default = HashMap::from([("af_heart".to_string(), 0)]);
    let Ok(value) = env::var("VOICE_TTS_VOICES_JSON") else {
        return Ok(default);
    };
    serde_json::from_str(&value).map_err(|_| ConfigError::InvalidVoiceMap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_configuration_enforces_inclusive_bounds() {
        const KEY: &str = "LUNARVOICE_TEST_RANGE";
        unsafe { env::set_var(KEY, "0") };
        assert!(matches!(
            env_number_in_range(KEY, 5u32, 1, 10),
            Err(ConfigError::OutOfRange(KEY))
        ));
        for value in ["1", "10"] {
            unsafe { env::set_var(KEY, value) };
            assert_eq!(
                env_number_in_range(KEY, 5u32, 1, 10).ok(),
                value.parse().ok()
            );
        }
        unsafe { env::set_var(KEY, "11") };
        assert!(matches!(
            env_number_in_range(KEY, 5u32, 1, 10),
            Err(ConfigError::OutOfRange(KEY))
        ));
        unsafe { env::set_var(KEY, "not-a-number") };
        assert!(matches!(
            env_number_in_range(KEY, 5u32, 1, 10),
            Err(ConfigError::InvalidInteger(KEY))
        ));
        unsafe { env::remove_var(KEY) };
    }

    #[test]
    fn stt_engine_env_parsing() {
        // One test keeps env mutation sequential; parallel tests must not
        // observe a partially-updated VOICE_STT_ENGINE.
        unsafe { env::remove_var("VOICE_STT_ENGINE") };
        assert_eq!(env_stt_engine().ok(), Some(SttEngineKind::Whisper));
        for value in ["parakeet", "parakeet-tdt", "transducer", "PARAKEET"] {
            unsafe { env::set_var("VOICE_STT_ENGINE", value) };
            assert_eq!(env_stt_engine().ok(), Some(SttEngineKind::Parakeet));
        }
        unsafe { env::set_var("VOICE_STT_ENGINE", "sensevoice") };
        assert!(env_stt_engine().is_err());
        unsafe { env::remove_var("VOICE_STT_ENGINE") };
    }

    #[test]
    fn stt_language_normalizes_auto_and_accepts_whisper_codes() {
        assert_eq!(normalize_stt_language("auto").ok(), Some(String::new()));
        assert_eq!(normalize_stt_language(" EN ").ok(), Some("en".into()));
        assert!(matches!(
            normalize_stt_language("english"),
            Err(ConfigError::InvalidSttLanguage)
        ));
    }

    #[test]
    fn parakeet_defaults_follow_model_name() {
        let paths = SttPathDefaults::new("parakeet-tdt-0.6b-v3-int8", SttEngineKind::Parakeet)
            .unwrap_or_else(|error| panic!("valid Parakeet model rejected: {error}"));
        let root = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
        assert_eq!(paths.encoder, format!("{root}/encoder.int8.onnx"));
        assert_eq!(paths.decoder, format!("{root}/decoder.int8.onnx"));
        assert_eq!(paths.joiner, format!("{root}/joiner.int8.onnx"));
        assert_eq!(paths.tokens, format!("{root}/tokens.txt"));
    }

    #[test]
    fn whisper_defaults_follow_model_size() {
        for size in ["tiny", "base", "small.en"] {
            let model = format!("whisper-{size}");
            let paths = SttPathDefaults::new(&model, SttEngineKind::Whisper)
                .unwrap_or_else(|error| panic!("valid Whisper model rejected: {error}"));
            let root = format!("sherpa-onnx-{model}");
            assert_eq!(paths.encoder, format!("{root}/{size}-encoder.int8.onnx"));
            assert_eq!(paths.decoder, format!("{root}/{size}-decoder.int8.onnx"));
            assert_eq!(paths.tokens, format!("{root}/{size}-tokens.txt"));
        }
    }

    #[test]
    fn stt_model_names_reject_path_components_and_invalid_whisper_names() {
        for model in ["", "../whisper-tiny", "whisper/tiny", "whisper tiny"] {
            assert!(SttPathDefaults::new(model, SttEngineKind::Whisper).is_err());
        }
        assert!(SttPathDefaults::new("tiny", SttEngineKind::Whisper).is_err());
        assert!(SttPathDefaults::new("whisper-", SttEngineKind::Whisper).is_err());
    }
}
