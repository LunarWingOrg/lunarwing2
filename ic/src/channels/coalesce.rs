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

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::time::Instant;

const DELIVERY_QUEUE_CAPACITY: usize = 64;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// Async flush callback invoked with the final merged content.
pub type FlushFn = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Buffering key: one pending message per agent/transport/channel/conversation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelKey {
    /// Sending agent identifier (e.g. own JID).
    pub agent_id: String,
    /// Transport/adapter identifier (e.g. `xmpp`, `darkirc`).
    pub transport_id: String,
    /// Destination channel/conversation (e.g. MUC JID).
    pub channel_id: String,
    /// Conversation/thread scope (may be empty).
    pub conversation_id: String,
}

impl ChannelKey {
    /// Build a key from its parts.
    pub fn new(
        agent_id: impl Into<String>,
        transport_id: impl Into<String>,
        channel_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            transport_id: transport_id.into(),
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

fn parse_window_duration(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() {
        return None;
    }
    Duration::try_from_secs_f64(seconds.max(0.1)).ok()
}

impl CoalesceConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(v) = std::env::var("COALESCE_ENABLED") {
            config.enabled = v.eq_ignore_ascii_case("true") || v == "1";
        }
        if let Some(window) = std::env::var("COALESCE_WINDOW_SEC")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .and_then(parse_window_duration)
        {
            config.window = window;
        }
        if let Some(n) = std::env::var("COALESCE_MAX_MERGES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
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

    /// Whether coalescing applies to this exact channel (case-insensitive),
    /// matching coordination hook scope.
    pub fn applies_to(&self, channel: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let channel = channel.to_lowercase();
        self.coordination_channels
            .iter()
            .any(|configured| channel == configured.to_lowercase())
    }
}

struct PendingMessage {
    content: String,
    merge_count: usize,
    flush: FlushFn,
    /// Invalidates stale timers when an entry is flushed early.
    generation: u64,
}

enum Command {
    Submit {
        key: ChannelKey,
        content: String,
        flush: FlushFn,
        response: oneshot::Sender<CoalesceDecision>,
    },
    FlushAll {
        response: oneshot::Sender<()>,
    },
    PendingCount {
        response: oneshot::Sender<usize>,
    },
}

struct CoalesceActor {
    config: CoalesceConfig,
    pending: HashMap<ChannelKey, PendingMessage>,
    deadlines: BinaryHeap<Reverse<(Instant, u64, ChannelKey)>>,
    next_generation: u64,
    delivery_timeout: Duration,
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

/// Pre-send coalesce buffer. Clone-cheap; all clones share one bounded actor.
#[derive(Clone)]
pub struct CoalesceBuffer {
    config: CoalesceConfig,
    commands: mpsc::Sender<Command>,
    /// Readers own active submissions. `flush_all` takes the writer so every
    /// submission started before shutdown is queued and processed first.
    delivery_barrier: Arc<RwLock<()>>,
    shutdown_timeout: Duration,
}

impl CoalesceBuffer {
    /// Create a buffer with the given config.
    pub fn new(config: CoalesceConfig) -> Self {
        Self::new_with_delivery_limits(
            config,
            DELIVERY_QUEUE_CAPACITY,
            DELIVERY_TIMEOUT,
            SHUTDOWN_TIMEOUT,
        )
    }

    fn new_with_delivery_limits(
        config: CoalesceConfig,
        queue_capacity: usize,
        delivery_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(queue_capacity.max(1));
        let actor = CoalesceActor {
            config: config.clone(),
            pending: HashMap::new(),
            deadlines: BinaryHeap::new(),
            next_generation: 0,
            delivery_timeout,
        };
        tokio::spawn(actor.run(receiver));
        Self {
            config,
            commands,
            delivery_barrier: Arc::new(RwLock::new(())),
            shutdown_timeout,
        }
    }

    /// Submit an outbound message for potential coalescing.
    ///
    /// `flush` is invoked by one bounded delivery actor when the window
    /// expires, on early flush, or from [`Self::flush_all`].
    ///
    /// One actor serializes lookup, merge, overflow handling, delivery, and
    /// timer expiry so concurrent submissions cannot replace each other.
    pub async fn submit(
        &self,
        key: ChannelKey,
        content: String,
        flush: FlushFn,
    ) -> CoalesceDecision {
        if !self.config.applies_to(&key.channel_id) || is_alert(&content) {
            return CoalesceDecision::SendNow(content);
        }

        // Establish submission ownership before queueing. Shutdown's write
        // guard waits for every submission that started before it.
        let _delivery_guard = self.delivery_barrier.read().await;
        let fallback = content.clone();
        let (response, result) = oneshot::channel();
        let command = Command::Submit {
            key,
            content,
            flush,
            response,
        };
        if self.commands.send(command).await.is_err() {
            return CoalesceDecision::SendNow(fallback);
        }
        result.await.unwrap_or(CoalesceDecision::SendNow(fallback))
    }

    /// Flush every pending message (thread termination / engine shutdown).
    pub async fn flush_all(&self) {
        if tokio::time::timeout(self.shutdown_timeout, self.flush_all_inner())
            .await
            .is_err()
        {
            tracing::warn!(
                timeout_ms = self.shutdown_timeout.as_millis(),
                "Timed out flushing coalesced responses during shutdown"
            );
        }
    }

    async fn flush_all_inner(&self) {
        let _delivery_guard = self.delivery_barrier.write().await;
        let (response, result) = oneshot::channel();
        if self
            .commands
            .send(Command::FlushAll { response })
            .await
            .is_ok()
        {
            let _ = result.await;
        }
    }

    /// Number of currently buffered messages (tests/diagnostics).
    pub async fn pending_count(&self) -> usize {
        let (response, result) = oneshot::channel();
        if self
            .commands
            .send(Command::PendingCount { response })
            .await
            .is_err()
        {
            return 0;
        }
        result.await.unwrap_or(0)
    }
}

impl CoalesceActor {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            self.remove_stale_deadlines();
            let next_deadline = self.deadlines.peek().map(|Reverse((at, _, _))| *at);
            let command = match next_deadline {
                Some(deadline) => {
                    tokio::select! {
                        command = commands.recv() => command,
                        _ = tokio::time::sleep_until(deadline) => {
                            self.flush_due().await;
                            continue;
                        }
                    }
                }
                None => commands.recv().await,
            };
            let Some(command) = command else {
                break;
            };
            self.handle_command(command).await;
        }
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Submit {
                key,
                content,
                flush,
                response,
            } => {
                self.handle_submit(key, content, flush).await;
                let _ = response.send(CoalesceDecision::Buffered);
            }
            Command::FlushAll { response } => {
                let mut pending = self.pending.drain().collect::<Vec<_>>();
                pending.sort_by_key(|(_, pending)| pending.generation);
                self.deadlines.clear();
                for (key, pending) in pending {
                    self.deliver(&key, pending).await;
                }
                let _ = response.send(());
            }
            Command::PendingCount { response } => {
                let _ = response.send(self.pending.len());
            }
        }
    }

    async fn handle_submit(&mut self, key: ChannelKey, content: String, flush: FlushFn) {
        let mut delivery = None;
        let mut insert_fresh = false;
        match self.pending.get_mut(&key) {
            Some(existing) => {
                let merged = merge_contents(&existing.content, &content);
                if merged.chars().count() > self.config.max_message_length {
                    delivery = self.pending.remove(&key);
                    insert_fresh = true;
                } else {
                    existing.content = merged;
                    existing.merge_count += 1;
                    if existing.merge_count >= self.config.max_merges {
                        delivery = self.pending.remove(&key);
                    }
                }
            }
            None => insert_fresh = true,
        }
        if insert_fresh {
            self.insert_fresh(key.clone(), content, flush);
        }
        if let Some(pending) = delivery {
            self.deliver(&key, pending).await;
        }
    }

    fn insert_fresh(&mut self, key: ChannelKey, content: String, flush: FlushFn) {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.pending.insert(
            key.clone(),
            PendingMessage {
                content,
                merge_count: 0,
                flush,
                generation,
            },
        );
        self.deadlines.push(Reverse((
            Instant::now() + self.config.window,
            generation,
            key,
        )));
    }

    fn remove_stale_deadlines(&mut self) {
        while let Some(Reverse((_, generation, key))) = self.deadlines.peek() {
            if self
                .pending
                .get(key)
                .is_some_and(|pending| pending.generation == *generation)
            {
                break;
            }
            self.deadlines.pop();
        }
    }

    async fn flush_due(&mut self) {
        let Some(Reverse((_, generation, key))) = self.deadlines.pop() else {
            return;
        };
        if self
            .pending
            .get(&key)
            .is_some_and(|pending| pending.generation == generation)
            && let Some(pending) = self.pending.remove(&key)
        {
            self.deliver(&key, pending).await;
        }
    }

    async fn deliver(&self, key: &ChannelKey, pending: PendingMessage) {
        let callback =
            std::panic::catch_unwind(AssertUnwindSafe(|| (pending.flush)(pending.content)));
        let Ok(callback) = callback else {
            tracing::error!(channel = %key.channel_id, "Coalesced delivery callback panicked");
            return;
        };
        match tokio::time::timeout(
            self.delivery_timeout,
            AssertUnwindSafe(callback).catch_unwind(),
        )
        .await
        {
            Err(_) => tracing::warn!(
                channel = %key.channel_id,
                timeout_ms = self.delivery_timeout.as_millis(),
                "Coalesced delivery timed out"
            ),
            Ok(Err(_)) => {
                tracing::error!(channel = %key.channel_id, "Coalesced delivery callback panicked");
            }
            Ok(Ok(())) => {}
        }
    }
}

/// `ALERT:` messages bypass the coalesce window.
fn is_alert(content: &str) -> bool {
    content
        .trim_start()
        .get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("alert:"))
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
    use tokio::sync::Mutex;

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
        ChannelKey::new("agent-1", "xmpp", "coord@conference.example", "thread-1")
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
    fn coalesce_window_parser_rejects_non_finite_and_overflow_values() {
        assert_eq!(parse_window_duration(0.0), Some(Duration::from_millis(100)));
        assert_eq!(
            parse_window_duration(0.05),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            parse_window_duration(2.5),
            Some(Duration::from_millis(2500))
        );
        assert_eq!(parse_window_duration(f64::NAN), None);
        assert_eq!(parse_window_duration(f64::INFINITY), None);
        assert_eq!(parse_window_duration(f64::MAX), None);
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

    #[test]
    fn test_scope_match_is_exact_and_case_insensitive() {
        let config = test_config();
        assert!(config.applies_to("COORD@CONFERENCE.EXAMPLE"));
        assert!(!config.applies_to("other-coord@conference.example"));
        assert!(!config.applies_to("xmpp:coord@conference.example/group"));
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
        let key = ChannelKey::new("agent-1", "xmpp", "general", "thread-1");
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
    async fn test_identical_destination_on_different_transports_stays_isolated() {
        let buffer = CoalesceBuffer::new(test_config());
        let (xmpp_flush, xmpp_sent) = collector();
        let (wasm_flush, wasm_sent) = collector();
        let destination = "coord@conference.example";
        buffer
            .submit(
                ChannelKey::new("agent-1", "xmpp", destination, "thread-1"),
                "STATUS: xmpp".into(),
                xmpp_flush,
            )
            .await;
        buffer
            .submit(
                ChannelKey::new("agent-1", "wasm-xmpp", destination, "thread-1"),
                "STATUS: wasm".into(),
                wasm_flush,
            )
            .await;

        assert_eq!(buffer.pending_count().await, 2);
        buffer.flush_all().await;
        assert_eq!(*xmpp_sent.lock().await, vec!["STATUS: xmpp"]);
        assert_eq!(*wasm_sent.lock().await, vec!["STATUS: wasm"]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_flush_all_waits_for_timer_callback() {
        let mut config = test_config();
        config.window = Duration::from_secs(1);
        let buffer = CoalesceBuffer::new(config);
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let flush: FlushFn = {
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            Arc::new(move |_| {
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                })
            })
        };
        buffer.submit(key(), "STATUS: timer".into(), flush).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        started.notified().await;

        let shutdown = {
            let buffer = buffer.clone();
            tokio::spawn(async move { buffer.flush_all().await })
        };
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        release.notify_one();
        shutdown.await.unwrap();
    }

    #[tokio::test]
    async fn test_flush_all_waits_for_early_flush_callback() {
        let mut config = test_config();
        config.max_merges = 1;
        config.window = Duration::from_secs(60);
        let buffer = CoalesceBuffer::new(config);
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let flush: FlushFn = {
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            Arc::new(move |_| {
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                })
            })
        };
        buffer
            .submit(key(), "STATUS: first".into(), flush.clone())
            .await;
        let early_submit = {
            let buffer = buffer.clone();
            tokio::spawn(async move { buffer.submit(key(), "STATUS: second".into(), flush).await })
        };
        started.notified().await;

        let shutdown = {
            let buffer = buffer.clone();
            tokio::spawn(async move { buffer.flush_all().await })
        };
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        release.notify_one();
        early_submit.await.unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn test_stalled_delivery_times_out_and_actor_continues() {
        let mut config = test_config();
        config.window = Duration::from_secs(1);
        let buffer = CoalesceBuffer::new_with_delivery_limits(
            config,
            2,
            Duration::from_secs(5),
            Duration::from_secs(30),
        );
        let started = Arc::new(tokio::sync::Notify::new());
        let flush: FlushFn = {
            let started = Arc::clone(&started);
            Arc::new(move |_| {
                let started = Arc::clone(&started);
                Box::pin(async move {
                    started.notify_one();
                    std::future::pending::<()>().await;
                })
            })
        };
        buffer.submit(key(), "STATUS: stalled".into(), flush).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        started.notified().await;

        tokio::time::advance(Duration::from_secs(5)).await;
        let (flush, sent) = collector();
        let next_key = ChannelKey::new("agent-1", "xmpp", "coord@conference.example", "thread-2");
        buffer.submit(next_key, "STATUS: next".into(), flush).await;
        buffer.flush_all().await;
        assert_eq!(*sent.lock().await, vec!["STATUS: next"]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_bounded_queue_backpressures_while_delivery_stalls() {
        let mut config = test_config();
        config.window = Duration::from_secs(1);
        let buffer = CoalesceBuffer::new_with_delivery_limits(
            config,
            1,
            Duration::from_secs(60),
            Duration::from_secs(120),
        );
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let stalled: FlushFn = {
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            Arc::new(move |_| {
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                })
            })
        };
        buffer
            .submit(key(), "STATUS: stalled".into(), stalled)
            .await;
        tokio::time::advance(Duration::from_secs(1)).await;
        started.notified().await;

        let (flush, _) = collector();
        let submit = |conversation: &str| {
            let buffer = buffer.clone();
            let flush = flush.clone();
            let key = ChannelKey::new("agent-1", "xmpp", "coord@conference.example", conversation);
            tokio::spawn(async move { buffer.submit(key, "STATUS: queued".into(), flush).await })
        };
        let queued = submit("thread-2");
        tokio::task::yield_now().await;
        let backpressured = submit("thread-3");
        tokio::task::yield_now().await;
        assert!(!queued.is_finished());
        assert!(!backpressured.is_finished());

        release.notify_one();
        queued.await.unwrap();
        backpressured.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn test_shutdown_finishes_after_stalled_delivery_timeout() {
        let mut config = test_config();
        config.window = Duration::from_secs(60);
        let buffer = CoalesceBuffer::new_with_delivery_limits(
            config,
            2,
            Duration::from_secs(5),
            Duration::from_secs(30),
        );
        let flush: FlushFn =
            Arc::new(move |_| Box::pin(async move { std::future::pending::<()>().await }));
        buffer.submit(key(), "STATUS: pending".into(), flush).await;

        let shutdown = {
            let buffer = buffer.clone();
            tokio::spawn(async move { buffer.flush_all().await })
        };
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        tokio::time::advance(Duration::from_secs(5)).await;
        shutdown.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn test_shutdown_has_aggregate_deadline() {
        let mut config = test_config();
        config.window = Duration::from_secs(60);
        let buffer = CoalesceBuffer::new_with_delivery_limits(
            config,
            2,
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let stalled: FlushFn =
            Arc::new(move |_| Box::pin(async move { std::future::pending::<()>().await }));
        buffer
            .submit(key(), "STATUS: first".into(), stalled.clone())
            .await;
        buffer
            .submit(
                ChannelKey::new("agent-1", "xmpp", "coord@conference.example", "thread-2"),
                "STATUS: second".into(),
                stalled,
            )
            .await;

        let shutdown = {
            let buffer = buffer.clone();
            tokio::spawn(async move { buffer.flush_all().await })
        };
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        tokio::time::advance(Duration::from_secs(5)).await;
        shutdown.await.unwrap();
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
