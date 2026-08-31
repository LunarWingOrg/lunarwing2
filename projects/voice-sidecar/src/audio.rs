use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone)]
pub struct AudioData {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    #[default]
    Wav,
    Ogg,
    Opus,
    Mp3,
    Mp4,
    Webm,
    Flac,
    M4a,
    Aac,
}

impl AudioFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Ogg => "ogg",
            Self::Opus => "opus",
            Self::Mp3 => "mp3",
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Flac => "flac",
            Self::M4a => "m4a",
            Self::Aac => "aac",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("audio data is empty")]
    Empty,
    #[error("decoded audio is {actual} bytes; limit is {limit} bytes")]
    TooLarge { actual: usize, limit: usize },
    #[error("audio duration exceeds {0} seconds")]
    TooLong(u64),
    #[error("invalid WAV data: {0}")]
    InvalidWav(String),
    #[error("ffmpeg is required to decode this audio format")]
    FfmpegUnavailable,
    #[error("audio conversion failed")]
    ConversionFailed,
    #[error("audio conversion timed out")]
    ConversionTimeout,
}

pub async fn decode_audio(
    bytes: Vec<u8>,
    format: AudioFormat,
    max_bytes: usize,
    max_secs: u64,
    ffmpeg_path: &Path,
    timeout: Duration,
) -> Result<AudioData, AudioError> {
    if bytes.is_empty() {
        return Err(AudioError::Empty);
    }
    if bytes.len() > max_bytes {
        return Err(AudioError::TooLarge {
            actual: bytes.len(),
            limit: max_bytes,
        });
    }
    if matches!(format, AudioFormat::Wav) {
        return tokio::task::spawn_blocking(move || decode_wav(&bytes, max_secs))
            .await
            .map_err(|_| AudioError::ConversionFailed)?;
    }
    decode_with_ffmpeg(bytes, format, max_secs, ffmpeg_path, timeout).await
}

fn decode_wav(bytes: &[u8], max_secs: u64) -> Result<AudioData, AudioError> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|error| AudioError::InvalidWav(error.to_string()))?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 {
        return Err(AudioError::InvalidWav(
            "invalid channel or sample rate".into(),
        ));
    }
    let frame_count = u64::from(reader.duration());
    if frame_count > max_secs.saturating_mul(u64::from(spec.sample_rate)) {
        return Err(AudioError::TooLong(max_secs));
    }

    let interleaved = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| AudioError::InvalidWav(error.to_string()))?,
        hound::SampleFormat::Int if spec.bits_per_sample <= 16 => {
            let scale = ((1i64 << spec.bits_per_sample.saturating_sub(1)) - 1) as f32;
            reader
                .samples::<i16>()
                .map(|sample| sample.map(|value| f32::from(value) / scale))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| AudioError::InvalidWav(error.to_string()))?
        }
        hound::SampleFormat::Int => {
            let scale = ((1i64 << spec.bits_per_sample.saturating_sub(1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 / scale))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| AudioError::InvalidWav(error.to_string()))?
        }
    };
    let mono = downmix(&interleaved, usize::from(spec.channels));
    Ok(AudioData {
        samples: resample_linear(&mono, spec.sample_rate, TARGET_SAMPLE_RATE),
        sample_rate: TARGET_SAMPLE_RATE,
    })
}

async fn decode_with_ffmpeg(
    bytes: Vec<u8>,
    format: AudioFormat,
    max_secs: u64,
    ffmpeg_path: &Path,
    timeout: Duration,
) -> Result<AudioData, AudioError> {
    let suffix = format!(".{}", format.extension());
    let input =
        tokio::task::spawn_blocking(move || -> Result<tempfile::NamedTempFile, AudioError> {
            let mut file = tempfile::Builder::new()
                .suffix(&suffix)
                .tempfile()
                .map_err(|_| AudioError::ConversionFailed)?;
            file.write_all(&bytes)
                .map_err(|_| AudioError::ConversionFailed)?;
            Ok(file)
        })
        .await
        .map_err(|_| AudioError::ConversionFailed)??;

    // Decode at most one second beyond the configured limit. The extra bounded
    // sentinel lets us distinguish an exact-limit input from an overlong input
    // instead of silently truncating it with ffmpeg's `-t` option.
    let probe_secs_arg = max_secs.saturating_add(1).to_string();
    let mut command = Command::new(PathBuf::from(ffmpeg_path));
    command
        .kill_on_drop(true)
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(input.path())
        .args([
            "-t",
            &probe_secs_arg,
            "-ac",
            "1",
            "-ar",
            "16000",
            "-f",
            "f32le",
            "pipe:1",
        ]);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| AudioError::ConversionTimeout)?
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AudioError::FfmpegUnavailable
            } else {
                AudioError::ConversionFailed
            }
        })?;
    if !output.status.success() {
        return Err(AudioError::ConversionFailed);
    }
    let samples = decode_f32le_output(&output.stdout, max_secs)?;
    Ok(AudioData {
        samples,
        sample_rate: TARGET_SAMPLE_RATE,
    })
}

fn decode_f32le_output(bytes: &[u8], max_secs: u64) -> Result<Vec<f32>, AudioError> {
    if bytes.is_empty() {
        return Err(AudioError::Empty);
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(AudioError::ConversionFailed);
    }
    let max_bytes = max_secs
        .saturating_mul(u64::from(TARGET_SAMPLE_RATE))
        .saturating_mul(4);
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(AudioError::TooLong(max_secs));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn downmix(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect()
}

fn resample_linear(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let output_len = samples.len().saturating_mul(target_rate as usize) / source_rate as usize;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * source_rate as f64 / target_rate as f64;
            let left = position.floor() as usize;
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            samples[left] * (1.0 - fraction) + samples[right] * fraction
        })
        .collect()
}

pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len().saturating_mul(2) as u32;
    let mut output = Vec::with_capacity(44 + data_len as usize);
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16u32.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&sample_rate.to_le_bytes());
    output.extend_from_slice(&sample_rate.saturating_mul(2).to_le_bytes());
    output.extend_from_slice(&2u16.to_le_bytes());
    output.extend_from_slice(&16u16.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        output.extend_from_slice(&value.to_le_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_round_trip_resamples_to_sixteen_khz() {
        let input = vec![0.25; 8_000];
        let wav = encode_wav(&input, 8_000);
        let decoded = decode_wav(&wav, 2).unwrap();
        assert_eq!(decoded.sample_rate, TARGET_SAMPLE_RATE);
        assert_eq!(decoded.samples.len(), 16_000);
        assert!((decoded.samples[100] - 0.25).abs() < 0.01);
    }

    #[test]
    fn wav_duration_limit_is_enforced() {
        let wav = encode_wav(&vec![0.0; 32_000], 16_000);
        assert!(matches!(decode_wav(&wav, 1), Err(AudioError::TooLong(1))));
    }

    #[test]
    fn ffmpeg_output_rejects_empty_malformed_and_overlong_audio() {
        assert!(matches!(
            decode_f32le_output(&[], 1),
            Err(AudioError::Empty)
        ));
        assert!(matches!(
            decode_f32le_output(&[0, 0, 0], 1),
            Err(AudioError::ConversionFailed)
        ));
        let exact_limit = vec![0; TARGET_SAMPLE_RATE as usize * 4];
        assert_eq!(
            decode_f32le_output(&exact_limit, 1)
                .map(|samples| samples.len())
                .ok(),
            Some(TARGET_SAMPLE_RATE as usize)
        );
        let over_limit = vec![0; TARGET_SAMPLE_RATE as usize * 4 + 4];
        assert!(matches!(
            decode_f32le_output(&over_limit, 1),
            Err(AudioError::TooLong(1))
        ));
    }
}
