//! Outbound coalesce window for coordination channels (Phase 4, stretch).
//!
//! `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`:
//! a pre-send buffer (default 2s) that batches rapid-fire messages from the
//! same agent to the same coordination channel, merging them into one
//! delivery. Lives in the channel adapter layer — AFTER BeforeOutbound
//! hooks, BEFORE the actual channel send.
//!
//! - Only active on configured coordination channels (`COORD_CHANNELS`).
//! - `ALERT:` messages bypass the window entirely (priority flush).
//! - Merging: same tag prefix → concatenate with `" | "`; different tags →
//!   keep the first message's tag; exact duplicates stripped; merges that
//!   would exceed the length cap flush the pending message first and start
//!   a fresh window.
//! - [`CoalesceBuffer::flush_all`] drains pending messages (thread
//!   termination / engine shutdown).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;

/// Async flush callback invoked with the final merged content.
pub type FlushFn = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Buffering key: one pending message per agent/channel/conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelKey {
    /// Sending agent identifier (e.g. own JID).
    pub agent_id: String,
    /// Destination channel/conversation (e.g. MUC JID).
    pub channel_id: String,
    /// Conversation/thread scope (may be empty).
    pub conversation_id: String,
}

impl ChannelKey {
    /// Build a key from its parts.
    pub fn new(
        agent_id: impl Into<String>,
        channel_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            channel_id: channel_id.into(),
            conversation_id: conversation_id.into(),
        }
    }
}

/// Tunables for the coalesce buffer.
#[derive(Debug, Clone)]
pub struct CoalesceConfig {
    /// Master switch (`COALESCE_ENABLED`, default true).
    pub enabled: bool,
    /// Merge window (`COALESCE_WINDOW_SEC`, default 2.0 seconds).
    pub window: Duration,
    /// Flush immediately after this many merges (`COALESCE_MAX_MERGES`, default 3).
    pub max_merges: usize,
    /// Merged content cap; overflow flushes pending first (default 420).
    pub max_message_length: usize,
    /// Channels (JIDs/names) where coalescing applies (`COORD_CHANNELS`).
    pub coordination_channels: Vec<String>,
}

impl Default for CoalesceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window: Duration::from_secs(2),
            max_merges: 3,
            max_message_length: 420,
            coordination_channels: Vec::new(),
        }
    }
}

impl CoalesceConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(v) = std::env::var("COALESCE_ENABLED") {
            config.enabled = v.eq_ignore_ascii_case("true") || v == "1";
        }
        if let Some(secs) = std::env::var("COALESCE_WINDOW_SEC")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
        {
            config.window = Duration::from_secs_f64(secs.max(0.1));
        }
        if let Some(n) = std::env::var("COALESCE_MAX_MERGES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            config.max_merges = n;
        }
        if let Ok(v) = std::env::var("COORD_CHANNELS") {
            config.coordination_channels = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        config
    }

    /// Whether coalescing applies to this channel (case-insensitive
    /// equality or containment, matching the coordination hooks).
    pub fn applies_to(&self, channel: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let channel = channel.to_lowercase();
        self.coordination_channels.iter().any(|c| {
            let c = c.to_lowercase();
            channel == c || channel.contains(&c)
        })
    }
}

struct PendingMessage {
    content: String,
    merge_count: usize,
    flush: FlushFn,
    /// Invalidates stale timers when an entry is flushed early.
    generation: u64,
}

/// Result of submitting a message to the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoalesceDecision {
    /// Deliver this content immediately (bypass or no buffering).
    SendNow(String),
    /// Message was buffered (or merged); delivery happens via the flush
    /// callback when the window expires.
    Buffered,
}

/// Pre-send coalesce buffer. Clone-cheap; all clones share the pending map.
#[derive(Clone)]
pub struct CoalesceBuffer {
    config: CoalesceConfig,
    pending: Arc<Mutex<HashMap<ChannelKey, PendingMessage>>>,
    /// Monotonic generation source for timer identity. Generations are never
    /// reused, so a stale timer can never match a replacement entry that was
    /// inserted after an early flush removed the original (ISSUE-005).
    next_generation: Arc<AtomicU64>,
}

impl CoalesceBuffer {
    /// Create a buffer with the given config.
    pub fn new(config: CoalesceConfig) -> Self {
        Self {
            config,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Submit an outbound message for potential coalescing.
    ///
    /// `flush` is invoked (on a spawned task) with the merged content when
    /// the window expires, on early flush, or from [`Self::flush_all`].
    ///
    /// Lookup, merge, overflow handling, and fresh insertion all happen
    /// under one map lock so concurrent first submissions for a key merge
    /// instead of replacing each other (ISSUE-005).
    pub async fn submit(
        &self,
        key: ChannelKey,
        content: String,
        flush: FlushFn,
    ) -> CoalesceDecision {
        if !self.config.applies_to(&key.channel_id) || is_alert(&content) {
            return CoalesceDecision::SendNow(content);
        }

        // Collected under the lock; flushed after it is released so delivery
        // never serializes on (or re-enters) the pending map.
        let mut early_flush: Option<(FlushFn, String)> = None;
        let mut arm_timer: Option<u64> = None;
        {
            let mut guard = self.pending.lock().await;
            match guard.get_mut(&key) {
                Some(existing) => {
                    let merged = merge_contents(&existing.content, &content);
                    if merged.chars().count() > self.config.max_message_length {
                        // Overflow: flush pending now, start a fresh window.
                        if let Some(taken) = guard.remove(&key) {
                            early_flush = Some((taken.flush, taken.content));
                        }
                        arm_timer = Some(self.insert_fresh(&mut guard, &key, content, flush));
                    } else {
                        existing.content = merged;
                        existing.merge_count += 1;
                        if existing.merge_count >= self.config.max_merges
                            && let Some(taken) = guard.remove(&key)
                        {
                            early_flush = Some((taken.flush, taken.content));
                        }
                    }
                }
                None => {
                    arm_timer = Some(self.insert_fresh(&mut guard, &key, content, flush));
                }
            }
        }

        if let Some((flush, content)) = early_flush {
            spawn_flush(flush, content);
        }
        if let Some(generation) = arm_timer {
            self.spawn_timer(key, generation);
        }
        CoalesceDecision::Buffered
    }

    /// Insert a fresh pending entry under the held map lock and return its
    /// globally unique generation.
    fn insert_fresh(
        &self,
        guard: &mut HashMap<ChannelKey, PendingMessage>,
        key: &ChannelKey,
        content: String,
        flush: FlushFn,
    ) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        guard.insert(
            key.clone(),
            PendingMessage {
                content,
                merge_count: 0,
                flush,
                generation,
            },
        );
        generation
    }

    /// Arm the window-expiry timer for a freshly inserted entry.
    fn spawn_timer(&self, key: ChannelKey, generation: u64) {
        let pending = Arc::clone(&self.pending);
        let window = self.config.window;
        tokio::spawn(async move {
            tokio::time::sleep(window).await;
            let taken = {
                let mut guard = pending.lock().await;
                match guard.get(&key) {
                    Some(p) if p.generation == generation => guard.remove(&key),
                    _ => None,
                }
            };
            if let Some(taken) = taken {
                (taken.flush)(taken.content).await;
            }
        });
    }

    /// Flush every pending message (thread termination / engine shutdown).
    pub async fn flush_all(&self) {
        let taken: Vec<PendingMessage> = {
            let mut guard = self.pending.lock().await;
            guard.drain().map(|(_, p)| p).collect()
        };
        for pending in taken {
            (pending.flush)(pending.content).await;
        }
    }

    /// Number of currently buffered messages (tests/diagnostics).
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }
}

/// `ALERT:` messages bypass the coalesce window.
fn is_alert(content: &str) -> bool {
    content
        .trim_start()
        .get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("alert:"))
}

fn spawn_flush(flush: FlushFn, content: String) {
    tokio::spawn(async move {
        flush(content).await;
    });
}

/// Split `"TAG: rest"` into `(Some("TAG"), "rest")` for uppercase tags.
fn split_tag(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim_start();
    match trimmed.find(':') {
        Some(i) if i > 0 && trimmed[..i].chars().all(|c| c.is_ascii_uppercase()) => {
            (Some(&trimmed[..i]), trimmed[i + 1..].trim_start())
        }
        _ => (None, trimmed),
    }
}

/// Merge two messages per the plan's strategy.
pub fn merge_contents(existing: &str, new: &str) -> String {
    if existing == new {
        // Exact duplicate within the window: stripped.
        return existing.to_string();
    }
    let (existing_tag, _) = split_tag(existing);
    let (new_tag, new_rest) = split_tag(new);
    match (existing_tag, new_tag) {
        (Some(a), Some(b)) if a == b => {
            if existing.contains(new_rest) {
                return existing.to_string();
            }
            format!("{existing} | {new_rest}")
        }
        _ => {
            if new.contains(existing) {
                return new.to_string();
            }
            format!("{existing} | {new}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_config() -> CoalesceConfig {
        CoalesceConfig {
            enabled: true,
            window: Duration::from_millis(50),
            max_merges: 3,
            max_message_length: 100,
            coordination_channels: vec!["coord@conference.example".to_string()],
        }
    }

    fn key() -> ChannelKey {
        ChannelKey::new("agent-1", "coord@conference.example", "thread-1")
    }

    fn collector() -> (FlushFn, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sent_clone = Arc::clone(&sent);
        let flush: FlushFn = Arc::new(move |content| {
            let sent = Arc::clone(&sent_clone);
            Box::pin(async move {
                sent.lock().await.push(content);
            })
        });
        (flush, sent)
    }

    #[test]
    fn test_merge_same_tag() {
        assert_eq!(
            merge_contents("STATUS: build started", "STATUS: 50% done"),
            "STATUS: build started | 50% done"
        );
    }

    #[test]
    fn test_merge_different_tags_keeps_first() {
        assert_eq!(
            merge_contents("STATUS: up", "QUERY: confirm?"),
            "STATUS: up | QUERY: confirm?"
        );
    }

    #[test]
    fn test_merge_strips_exact_duplicate() {
        assert_eq!(merge_contents("STATUS: up", "STATUS: up"), "STATUS: up");
    }

    #[test]
    fn test_split_tag() {
        assert_eq!(split_tag("STATUS: hello"), (Some("STATUS"), "hello"));
        assert_eq!(split_tag("not tagged: x"), (None, "not tagged: x"));
        assert_eq!(split_tag(": leading colon"), (None, ": leading colon"));
    }

    #[test]
    fn test_is_alert() {
        assert!(is_alert("ALERT: disk full"));
        assert!(is_alert("  alert: disk full"));
        assert!(!is_alert("STATUS: all good"));
    }

    #[tokio::test]
    async fn test_rapid_fire_messages_are_merged() {
        let buffer = CoalesceBuffer::new(test_config());
        let (flush, sent) = collector();
        assert_eq!(
            buffer
                .submit(key(), "STATUS: one".into(), flush.clone())
                .await,
            CoalesceDecision::Buffered
        );
        assert_eq!(
            buffer
                .submit(key(), "STATUS: two".into(), flush.clone())
                .await,
            CoalesceDecision::Buffered
        );
        // Wait past the window.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let delivered = sent.lock().await.clone();
        assert_eq!(delivered, vec!["STATUS: one | two".to_string()]);
    }

    #[tokio::test]
    async fn test_alert_bypasses_coalesce() {
        let buffer = CoalesceBuffer::new(test_config());
        let (flush, _sent) = collector();
        let decision = buffer.submit(key(), "ALERT: urgent".into(), flush).await;
        assert_eq!(
            decision,
            CoalesceDecision::SendNow("ALERT: urgent".to_string())
        );
        assert_eq!(buffer.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_non_coordination_channel_unaffected() {
        let buffer = CoalesceBuffer::new(test_config());
        let (flush, _sent) = collector();
        let key = ChannelKey::new("agent-1", "general", "thread-1");
        let decision = buffer.submit(key, "STATUS: hi".into(), flush).await;
        assert!(matches!(decision, CoalesceDecision::SendNow(_)));
        assert_eq!(buffer.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_max_merges_flushes_immediately() {
        let mut config = test_config();
        config.max_merges = 2;
        let buffer = CoalesceBuffer::new(config);
        let (flush, sent) = collector();
        for msg in ["STATUS: a", "STATUS: b", "STATUS: c"] {
            buffer.submit(key(), msg.into(), flush.clone()).await;
        }
        // Third submit hits max_merges and flushes without waiting.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let delivered = sent.lock().await.clone();
        assert_eq!(delivered, vec!["STATUS: a | b | c".to_string()]);
    }

    #[tokio::test]
    async fn test_overflow_flushes_pending_then_buffers_new() {
        let buffer = CoalesceBuffer::new(test_config());
        let (flush, sent) = collector();
        let long = format!("STATUS: {}", "x".repeat(80));
        buffer.submit(key(), long, flush.clone()).await;
        let overflow = format!("STATUS: {}", "y".repeat(50));
        buffer.submit(key(), overflow.clone(), flush.clone()).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        let delivered = sent.lock().await.clone();
        assert_eq!(delivered.len(), 2);
        assert!(delivered[0].contains('x'));
        assert!(delivered[1] == overflow);
    }

    #[tokio::test]
    async fn test_flush_all_on_shutdown() {
        let buffer = CoalesceBuffer::new(test_config());
        let (flush, sent) = collector();
        buffer.submit(key(), "STATUS: pending".into(), flush).await;
        buffer.flush_all().await;
        let delivered = sent.lock().await.clone();
        assert_eq!(delivered, vec!["STATUS: pending".to_string()]);
        assert_eq!(buffer.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_disabled_config_passes_through() {
        let mut config = test_config();
        config.enabled = false;
        let buffer = CoalesceBuffer::new(config);
        let (flush, _sent) = collector();
        let decision = buffer.submit(key(), "STATUS: hi".into(), flush).await;
        assert!(matches!(decision, CoalesceDecision::SendNow(_)));
    }

    #[tokio::test]
    async fn test_window_timer_uses_single_delivery() {
        // Ensure the timer doesn't double-deliver when flush_all raced it.
        let buffer = CoalesceBuffer::new(test_config());
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);
        let flush: FlushFn = Arc::new(move |_content| {
            let calls = Arc::clone(&calls_clone);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        });
        buffer.submit(key(), "STATUS: once".into(), flush).await;
        buffer.flush_all().await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_concurrent_first_submissions_merge_not_lose() {
        // ISSUE-005 regression: two concurrent first submissions for one key
        // must merge atomically; neither message may be dropped.
        let buffer = CoalesceBuffer::new(test_config());
        let (flush, sent) = collector();
        let (d1, d2) = tokio::join!(
            buffer.submit(key(), "STATUS: one".into(), flush.clone()),
            buffer.submit(key(), "STATUS: two".into(), flush.clone()),
        );
        assert_eq!(d1, CoalesceDecision::Buffered);
        assert_eq!(d2, CoalesceDecision::Buffered);
        // Exactly one pending entry holds both messages.
        assert_eq!(buffer.pending_count().await, 1);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let delivered = sent.lock().await.clone();
        assert_eq!(delivered.len(), 1, "exactly one merged delivery expected");
        assert!(delivered[0].contains("one"));
        assert!(delivered[0].contains("two"));
    }

    #[tokio::test]
    async fn test_stale_timer_cannot_remove_reinserted_entry() {
        // ISSUE-005 regression: after an early flush removes an entry, a
        // reinserted entry must survive the old entry's window timer.
        let mut config = test_config();
        config.window = Duration::from_millis(100);
        let buffer = CoalesceBuffer::new(config);
        let (flush, sent) = collector();
        buffer
            .submit(key(), "STATUS: old".into(), flush.clone())
            .await;
        // Early flush: "old" is delivered now, but its window timer is armed.
        buffer.flush_all().await;
        // Reinsert halfway through the old window; the new entry gets a
        // fresh generation, so the old timer (due at ~100ms from the first
        // submit) must ignore it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        buffer
            .submit(key(), "STATUS: new".into(), flush.clone())
            .await;
        // At ~120ms: old timer has fired, the new entry's timer has not.
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert_eq!(
            buffer.pending_count().await,
            1,
            "stale timer must not remove the reinserted entry"
        );
        // The new entry's own timer delivers it exactly once.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let delivered = sent.lock().await.clone();
        assert_eq!(
            delivered,
            vec!["STATUS: old".to_string(), "STATUS: new".to_string()]
        );
    }
}
