use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::llm::error::LlmError;
use crate::llm::provider::{OutputCorruptionKind, StreamFailureReason, TokenUsage};

const SNAPSHOT_SCHEMA_VERSION: u8 = 1;
const MAX_PROVIDER_CHARS: usize = 128;

pub(crate) struct StreamTelemetry {
    path: PathBuf,
    snapshot: Mutex<StreamTelemetrySnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StreamTelemetrySnapshot {
    schema_version: u8,
    updated_at_unix_seconds: u64,
    providers: BTreeMap<String, ProviderStreamMetrics>,
}

impl Default for StreamTelemetrySnapshot {
    fn default() -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            updated_at_unix_seconds: 0,
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProviderStreamMetrics {
    streams_started_total: u64,
    streams_completed_total: u64,
    retries_total: u64,
    stream_aborts_total: u64,
    decode_failures_total: u64,
    corruption_total: u64,
    #[serde(default)]
    repl_argument_corruption_total: u64,
    /// Tokens providers billed for successful-but-empty stream attempts that
    /// a decorator discarded and recovered past (WI#297, MR !1395 F-2).
    /// Recorded when the discarded usage is forwarded on a `Retried`
    /// observation or an escalation, so the counters measure recovered waste
    /// exactly once per discarded attempt.
    #[serde(default)]
    empty_stream_discarded_input_tokens_total: u64,
    #[serde(default)]
    empty_stream_discarded_output_tokens_total: u64,
    ttfc_samples_total: u64,
    ttfc_sum_ms: u64,
    ttfc_max_ms: u64,
    ttfc_last_ms: u64,
    last_issue_unix_seconds: u64,
    retry_reasons: BTreeMap<StreamFailureReason, u64>,
    abort_reasons: BTreeMap<StreamFailureReason, u64>,
    corruption_signatures: BTreeMap<OutputCorruptionKind, u64>,
}

impl StreamTelemetry {
    pub(crate) fn from_env() -> Self {
        let path = crate::config::helpers::env_or_override("LUNARWING_BASE_DIR")
            .or_else(|| crate::config::helpers::env_or_override("IRONCLAW_BASE_DIR"))
            .or_else(|| {
                crate::config::helpers::env_or_override("HOME")
                    .map(|home| format!("{home}/.lunarwing"))
            })
            .unwrap_or_else(|| ".lunarwing".to_string());
        Self::new(PathBuf::from(path).join("runtime/llm-stream-metrics.json"))
    }

    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            snapshot: Mutex::new(StreamTelemetrySnapshot::default()),
        }
    }

    pub(crate) async fn record_started(&self, provider: &str) {
        let mut snapshot = self.snapshot.lock().await;
        let metrics = provider_metrics(&mut snapshot, provider);
        metrics.streams_started_total = metrics.streams_started_total.saturating_add(1);
    }

    pub(crate) async fn record_retry(&self, provider: &str, reason: StreamFailureReason) {
        let mut snapshot = self.snapshot.lock().await;
        let metrics = provider_metrics(&mut snapshot, provider);
        metrics.retries_total = metrics.retries_total.saturating_add(1);
        increment(&mut metrics.retry_reasons, reason);
        record_failure(metrics, reason);
        metrics.last_issue_unix_seconds = unix_timestamp();
        snapshot.updated_at_unix_seconds = unix_timestamp();
        self.persist(&snapshot).await;
    }

    pub(crate) async fn record_completed(
        &self,
        provider: &str,
        ttfc: Option<Duration>,
        corruptions: &[OutputCorruptionKind],
    ) {
        let mut snapshot = self.snapshot.lock().await;
        let metrics = provider_metrics(&mut snapshot, provider);
        metrics.streams_completed_total = metrics.streams_completed_total.saturating_add(1);
        if let Some(duration) = ttfc {
            record_ttfc(metrics, duration);
        }
        if !corruptions.is_empty() {
            metrics.last_issue_unix_seconds = unix_timestamp();
        }
        for kind in corruptions {
            metrics.corruption_total = metrics.corruption_total.saturating_add(1);
            increment(&mut metrics.corruption_signatures, *kind);
        }
        snapshot.updated_at_unix_seconds = unix_timestamp();
        self.persist(&snapshot).await;
    }

    pub(crate) async fn record_abort(
        &self,
        provider: &str,
        reason: StreamFailureReason,
        ttfc: Option<Duration>,
    ) {
        let mut snapshot = self.snapshot.lock().await;
        let metrics = provider_metrics(&mut snapshot, provider);
        metrics.stream_aborts_total = metrics.stream_aborts_total.saturating_add(1);
        increment(&mut metrics.abort_reasons, reason);
        record_failure(metrics, reason);
        if let Some(duration) = ttfc {
            record_ttfc(metrics, duration);
        }
        metrics.last_issue_unix_seconds = unix_timestamp();
        snapshot.updated_at_unix_seconds = unix_timestamp();
        self.persist(&snapshot).await;
    }

    pub(crate) async fn record_repl_argument_corruption(&self, provider: &str) {
        let mut snapshot = self.snapshot.lock().await;
        let metrics = provider_metrics(&mut snapshot, provider);
        metrics.repl_argument_corruption_total =
            metrics.repl_argument_corruption_total.saturating_add(1);
        metrics.last_issue_unix_seconds = unix_timestamp();
        snapshot.updated_at_unix_seconds = unix_timestamp();
        self.persist(&snapshot).await;
    }

    /// Record tokens a provider billed for an empty stream attempt that a
    /// decorator discarded and recovered past (WI#297, MR !1395 F-2).
    pub(crate) async fn record_discarded_empty_usage(&self, provider: &str, usage: TokenUsage) {
        if usage.total() == 0 {
            return;
        }
        let mut snapshot = self.snapshot.lock().await;
        let metrics = provider_metrics(&mut snapshot, provider);
        metrics.empty_stream_discarded_input_tokens_total = metrics
            .empty_stream_discarded_input_tokens_total
            .saturating_add(u64::from(usage.input_tokens));
        metrics.empty_stream_discarded_output_tokens_total = metrics
            .empty_stream_discarded_output_tokens_total
            .saturating_add(u64::from(usage.output_tokens));
        metrics.last_issue_unix_seconds = unix_timestamp();
        snapshot.updated_at_unix_seconds = unix_timestamp();
        self.persist(&snapshot).await;
    }

    async fn persist(&self, snapshot: &StreamTelemetrySnapshot) {
        if let Err(error) = persist_snapshot(&self.path, snapshot).await {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "Failed to persist LLM stream telemetry"
            );
        }
    }
}

fn provider_metrics<'a>(
    snapshot: &'a mut StreamTelemetrySnapshot,
    provider: &str,
) -> &'a mut ProviderStreamMetrics {
    snapshot
        .providers
        .entry(provider_key(provider))
        .or_default()
}

fn provider_key(provider: &str) -> String {
    let provider = provider.trim();
    if provider.is_empty() {
        return "unknown".to_string();
    }
    if provider.chars().count() <= MAX_PROVIDER_CHARS {
        return provider.to_string();
    }

    let mut hasher = DefaultHasher::new();
    provider.hash(&mut hasher);
    let prefix = provider.chars().take(96).collect::<String>();
    format!("{prefix}~{:016x}", hasher.finish())
}

fn increment<K: Ord + Copy>(counts: &mut BTreeMap<K, u64>, key: K) {
    let count = counts.entry(key).or_default();
    *count = count.saturating_add(1);
}

fn record_failure(metrics: &mut ProviderStreamMetrics, reason: StreamFailureReason) {
    if reason == StreamFailureReason::Decode {
        metrics.decode_failures_total = metrics.decode_failures_total.saturating_add(1);
    }
}

fn record_ttfc(metrics: &mut ProviderStreamMetrics, duration: Duration) {
    let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    metrics.ttfc_samples_total = metrics.ttfc_samples_total.saturating_add(1);
    metrics.ttfc_sum_ms = metrics.ttfc_sum_ms.saturating_add(millis);
    metrics.ttfc_max_ms = metrics.ttfc_max_ms.max(millis);
    metrics.ttfc_last_ms = millis;
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn persist_snapshot(path: &Path, snapshot: &StreamTelemetrySnapshot) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let data = serde_json::to_vec_pretty(snapshot).map_err(std::io::Error::other)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("llm-stream-metrics.json");
    let temporary = path.with_file_name(format!(".{file_name}.tmp"));
    tokio::fs::write(&temporary, data).await?;
    tokio::fs::rename(temporary, path).await
}

pub(crate) fn classify_stream_failure(error: &LlmError) -> StreamFailureReason {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("decod") || message.contains("malformed stream") {
        return StreamFailureReason::Decode;
    }
    if message.contains("timeout") || message.contains("timed out") || message.contains("deadline")
    {
        return StreamFailureReason::Timeout;
    }

    match error {
        LlmError::RateLimited { .. } => StreamFailureReason::RateLimited,
        LlmError::EmptyStream { .. } => StreamFailureReason::Empty,
        LlmError::InvalidResponse { .. } | LlmError::Json(_) => StreamFailureReason::InvalidOutput,
        LlmError::ReplArgumentCorruption { .. } => StreamFailureReason::InvalidOutput,
        LlmError::RequestFailed { .. } | LlmError::Http(_) | LlmError::Io(_) => {
            StreamFailureReason::Transport
        }
        LlmError::AuthFailed { .. }
        | LlmError::SessionExpired { .. }
        | LlmError::SessionRenewalFailed { .. }
        | LlmError::ContextLengthExceeded { .. }
        | LlmError::ModelNotAvailable { .. } => StreamFailureReason::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_decode_and_timeout_failures() {
        assert_eq!(
            classify_stream_failure(&LlmError::RequestFailed {
                provider: "test".into(),
                reason: "error decoding response body".into(),
            }),
            StreamFailureReason::Decode
        );
        assert_eq!(
            classify_stream_failure(&LlmError::RequestFailed {
                provider: "test".into(),
                reason: "turn deadline elapsed".into(),
            }),
            StreamFailureReason::Timeout
        );
    }

    // WI#297: typed empty-stream errors classify as Empty without relying on
    // message substrings.
    #[test]
    fn classifies_empty_stream_failures_as_empty() {
        assert_eq!(
            classify_stream_failure(&crate::llm::streaming::empty_stream_error(
                "test", None, None
            )),
            StreamFailureReason::Empty
        );
        assert_eq!(
            classify_stream_failure(&crate::llm::streaming::empty_stream_error(
                "test",
                Some("stop"),
                None
            )),
            StreamFailureReason::Empty
        );
    }

    #[tokio::test]
    async fn retry_marks_provider_as_recently_unhealthy() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("metrics.json");
        let telemetry = StreamTelemetry::new(path.clone());

        telemetry
            .record_retry("provider-a", StreamFailureReason::Transport)
            .await;

        let snapshot: StreamTelemetrySnapshot =
            serde_json::from_slice(&tokio::fs::read(path).await.expect("telemetry snapshot"))
                .expect("valid telemetry JSON");
        let metrics = snapshot
            .providers
            .get("provider-a")
            .expect("provider metrics");
        assert_eq!(metrics.retries_total, 1);
        assert!(metrics.last_issue_unix_seconds > 0);
    }

    #[tokio::test]
    async fn persists_per_provider_stream_metrics_atomically() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("runtime/llm-stream-metrics.json");
        let telemetry = StreamTelemetry::new(path.clone());

        telemetry.record_started("provider-a").await;
        telemetry
            .record_retry("provider-a", StreamFailureReason::Decode)
            .await;
        telemetry
            .record_completed(
                "provider-a",
                Some(Duration::from_millis(125)),
                &[OutputCorruptionKind::RepeatedPhrase],
            )
            .await;

        let snapshot: StreamTelemetrySnapshot =
            serde_json::from_slice(&tokio::fs::read(path).await.expect("telemetry snapshot"))
                .expect("valid telemetry JSON");
        let metrics = snapshot
            .providers
            .get("provider-a")
            .expect("provider metrics");
        assert_eq!(metrics.streams_started_total, 1);
        assert_eq!(metrics.streams_completed_total, 1);
        assert_eq!(metrics.retries_total, 1);
        assert_eq!(metrics.decode_failures_total, 1);
        assert_eq!(metrics.corruption_total, 1);
        assert_eq!(metrics.ttfc_last_ms, 125);
        assert!(metrics.last_issue_unix_seconds > 0);
        assert_eq!(
            metrics.retry_reasons.get(&StreamFailureReason::Decode),
            Some(&1)
        );
        assert_eq!(
            metrics
                .corruption_signatures
                .get(&OutputCorruptionKind::RepeatedPhrase),
            Some(&1)
        );
        assert!(
            !directory
                .path()
                .join("runtime/.llm-stream-metrics.json.tmp")
                .exists()
        );
    }
}
