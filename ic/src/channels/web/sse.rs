//! SSE connection manager for broadcasting events to browser tabs.
//!
//! Events caused by a gateway message carry the message's correlation ids
//! (TUI design 4.2.6): [`SseManager::broadcast_correlated`] reads them from the
//! message metadata, and the SSE stream merges them into the event JSON as
//! top-level `message_id` and `client_turn_id` fields. WebSocket subscribers
//! ([`SseManager::subscribe_raw`]) receive the plain events.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use lunarwing_common::client_caps::is_valid_client_id;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use uuid::Uuid;

use crate::channels::web::types::SseEvent;

/// Maximum number of concurrent SSE/WebSocket connections.
/// Prevents resource exhaustion from connection flooding.
const MAX_CONNECTIONS: u64 = 100;

/// Interval of the empty keep-alive comment on idle SSE streams.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// The ids of the gateway message that caused an event (TUI design 4.1, rule 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correlation {
    /// Server id of the message (`IncomingMessage::id`, also returned by `POST /api/chat/send`).
    pub message_id: String,
    /// The client's id for the turn, when it sent one.
    pub client_turn_id: Option<String>,
}

impl Correlation {
    /// Reads the ids the gateway handlers stamp into message metadata.
    ///
    /// `None` unless `message_id` is a UUID (it is re-rendered in canonical form). A
    /// `client_turn_id` that fails `is_valid_client_id` is dropped, never echoed: the
    /// handlers reject such ids, so this is defence in depth for other producers.
    pub fn from_metadata(meta: &serde_json::Value) -> Option<Self> {
        let message_id = meta.get("message_id")?.as_str()?;
        let message_id = Uuid::parse_str(message_id).ok()?.to_string();
        let client_turn_id = meta
            .get("client_turn_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| is_valid_client_id(id))
            .map(str::to_string);
        Some(Self {
            message_id,
            client_turn_id,
        })
    }
}

/// A broadcast event together with the correlation of the message that caused it.
#[derive(Debug, Clone)]
pub struct CorrelatedEvent {
    pub event: SseEvent,
    pub correlation: Option<Correlation>,
}

/// Envelope for broadcast events: carries an optional user scope.
///
/// `user_id = None` means the event is global (e.g. Heartbeat) and delivered
/// to all subscribers. `user_id = Some(id)` means the event is only delivered
/// to subscribers that match that user_id.
#[derive(Debug, Clone)]
pub(crate) struct ScopedEvent {
    pub(crate) user_id: Option<String>,
    pub(crate) event: SseEvent,
    /// Ids merged into the SSE payload; `None` for uncorrelated broadcasts.
    pub(crate) correlation: Option<Correlation>,
}

/// Manages SSE broadcast to all connected browser tabs.
///
/// In multi-user mode, events are scoped by user_id so that each subscriber
/// only receives events intended for their user (plus global events like
/// Heartbeat). In single-user mode, all events are delivered to all subscribers
/// (backwards compatible).
pub struct SseManager {
    tx: broadcast::Sender<ScopedEvent>,
    connection_count: Arc<AtomicU64>,
    max_connections: u64,
}

impl SseManager {
    /// Create a new SSE manager.
    pub fn new() -> Self {
        // Buffer 256 events; lagging subscribers receive StreamGap before retained events.
        let (tx, _) = broadcast::channel(256);
        Self {
            tx,
            connection_count: Arc::new(AtomicU64::new(0)),
            max_connections: MAX_CONNECTIONS,
        }
    }

    /// Create an SSE manager that reuses an existing broadcast sender.
    ///
    /// This preserves the broadcast channel across `rebuild_state` calls so
    /// that sender handles captured by other components remain valid.
    ///
    /// **Important:** The connection counter is reset to zero. This method must
    /// only be called before the server starts accepting connections (i.e.,
    /// during startup wiring). Calling it after connections are established
    /// will break connection tracking and allow exceeding `MAX_CONNECTIONS`.
    pub(crate) fn from_sender(tx: broadcast::Sender<ScopedEvent>) -> Self {
        Self {
            tx,
            connection_count: Arc::new(AtomicU64::new(0)),
            max_connections: MAX_CONNECTIONS,
        }
    }

    /// Get a clone of the broadcast sender for use by other components.
    pub(crate) fn sender(&self) -> broadcast::Sender<ScopedEvent> {
        self.tx.clone()
    }

    /// Broadcast an event to all connected clients (global/unscoped).
    pub fn broadcast(&self, event: impl Into<SseEvent>) {
        self.send(None, event.into(), None);
    }

    /// Broadcast an event scoped to a specific user.
    ///
    /// Only subscribers for this user_id (or unscoped subscribers) will
    /// receive the event.
    pub fn broadcast_for_user(&self, user_id: &str, event: impl Into<SseEvent>) {
        self.send(Some(user_id), event.into(), None);
    }

    /// Broadcast an event caused by the message whose `metadata` is given.
    ///
    /// The event is scoped like [`Self::broadcast_for_user`] (global when `user_id` is
    /// `None`) and carries the message's [`Correlation`], when the metadata has one.
    pub fn broadcast_correlated(
        &self,
        user_id: Option<&str>,
        metadata: &serde_json::Value,
        event: impl Into<SseEvent>,
    ) {
        self.send(user_id, event.into(), Correlation::from_metadata(metadata));
    }

    fn send(&self, user_id: Option<&str>, event: SseEvent, correlation: Option<Correlation>) {
        let _ = self.tx.send(ScopedEvent {
            user_id: user_id.map(str::to_string),
            event,
            correlation,
        });
    }

    /// Get current number of active connections.
    pub fn connection_count(&self) -> u64 {
        self.connection_count.load(Ordering::Relaxed)
    }

    /// Create a raw broadcast subscription for non-SSE consumers (e.g. WebSocket).
    ///
    /// When `user_id` is `Some`, only events scoped to that user (or global
    /// events) are delivered. When `None`, all events are delivered (single-user
    /// backwards compatibility). Correlation ids are not included.
    ///
    /// Returns `None` if the maximum connection limit has been reached.
    pub fn subscribe_raw(
        &self,
        user_id: Option<String>,
    ) -> Option<impl Stream<Item = SseEvent> + Send + 'static + use<>> {
        let counter = self.reserve_connection()?;
        let inner = self.scoped_events(user_id).map(|item| item.event);
        Some(CountedStream { inner, counter })
    }

    /// Like [`Self::subscribe_raw`], but each event keeps its [`Correlation`].
    pub fn subscribe_raw_correlated(
        &self,
        user_id: Option<String>,
    ) -> Option<impl Stream<Item = CorrelatedEvent> + Send + 'static + use<>> {
        let counter = self.reserve_connection()?;
        let inner = self.scoped_events(user_id);
        Some(CountedStream { inner, counter })
    }

    /// Create a new SSE stream for a client connection.
    ///
    /// When `user_id` is `Some`, only events for that user (or global events)
    /// are delivered. When `None`, all events are delivered.
    ///
    /// Returns `None` if the maximum connection limit has been reached.
    pub fn subscribe(
        &self,
        user_id: Option<String>,
    ) -> Option<Sse<impl Stream<Item = Result<Event, Infallible>> + Send + 'static + use<>>> {
        self.subscribe_until(user_id, std::future::pending())
    }

    /// Like [`Self::subscribe`], but the stream ends as soon as `until` resolves, even
    /// with events still queued (used to cut off a revoked credential's stream).
    ///
    /// Returns `None` if the maximum connection limit has been reached.
    pub fn subscribe_until<F>(
        &self,
        user_id: Option<String>,
        until: F,
    ) -> Option<Sse<impl Stream<Item = Result<Event, Infallible>> + Send + 'static + use<F>>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let counter = self.reserve_connection()?;
        let frames = self.scoped_events(user_id).filter_map(sse_frame);
        let inner = futures::StreamExt::take_until(frames, Box::pin(until));
        Some(
            Sse::new(CountedStream { inner, counter })
                .keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL).text("")),
        )
    }

    /// Takes one connection slot; `None` when all `max_connections` are in use.
    ///
    /// The increment is atomic, so concurrent callers cannot overshoot the limit.
    /// The returned counter is decremented when the subscription is dropped.
    fn reserve_connection(&self) -> Option<Arc<AtomicU64>> {
        let max = self.max_connections;
        self.connection_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (current < max).then_some(current + 1)
            })
            .ok()?;
        Some(Arc::clone(&self.connection_count))
    }

    /// The events visible to a subscriber for `user_id`; lag is reported as `StreamGap`.
    fn scoped_events(
        &self,
        user_id: Option<String>,
    ) -> impl Stream<Item = CorrelatedEvent> + Send + Unpin + 'static + use<> {
        BroadcastStream::new(self.tx.subscribe()).filter_map(move |result| match result {
            Ok(scoped) => {
                let visible = visible_to(user_id.as_deref(), scoped.user_id.as_deref());
                visible.then_some(CorrelatedEvent {
                    event: scoped.event,
                    correlation: scoped.correlation,
                })
            }
            Err(BroadcastStreamRecvError::Lagged(missed_events)) => {
                tracing::warn!(
                    missed_events,
                    "Web event subscriber lagged; signaling stream gap"
                );
                Some(CorrelatedEvent {
                    event: SseEvent::StreamGap { missed_events },
                    correlation: None,
                })
            }
        })
    }
}

impl Default for SseManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Global events reach every subscriber, an unscoped subscriber receives every
/// event, and a user-scoped event reaches only that user's subscribers.
fn visible_to(subscriber: Option<&str>, scope: Option<&str>) -> bool {
    match (subscriber, scope) {
        (_, None) | (None, _) => true,
        (Some(subscriber), Some(scope)) => subscriber == scope,
    }
}

/// One SSE frame: `event: <type>` plus the JSON payload on one `data:` line.
fn sse_frame(item: CorrelatedEvent) -> Option<Result<Event, Infallible>> {
    let name = item.event.event_name();
    match sse_payload(&item.event, item.correlation.as_ref()) {
        Ok(data) => Some(Ok(Event::default().event(name).data(data))),
        Err(error) => {
            tracing::warn!(%error, event = name, "Failed to serialize SSE event");
            None
        }
    }
}

/// The event's own fields followed by the correlation ids, as one JSON object.
#[derive(Serialize)]
struct CorrelatedPayload<'a> {
    #[serde(flatten)]
    event: &'a SseEvent,
    message_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_turn_id: Option<&'a str>,
}

/// JSON payload of one SSE frame.
///
/// Uncorrelated events serialize exactly as before. A correlated event gains top-level
/// `message_id` and, when the client sent one, `client_turn_id`, after its own fields,
/// so `type` stays first and large payloads (image data URLs) are serialized once.
fn sse_payload(
    event: &SseEvent,
    correlation: Option<&Correlation>,
) -> Result<String, serde_json::Error> {
    match correlation {
        None => serde_json::to_string(event),
        Some(correlation) => serde_json::to_string(&CorrelatedPayload {
            event,
            message_id: &correlation.message_id,
            client_turn_id: correlation.client_turn_id.as_deref(),
        }),
    }
}

/// Stream wrapper that decrements connection count on drop.
///
/// When the SSE client disconnects, this stream is dropped
/// and the counter is decremented.
struct CountedStream<S> {
    inner: S,
    counter: Arc<AtomicU64>,
}

impl<S: Stream + Unpin> Stream for CountedStream<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S> Drop for CountedStream<S> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;

    use super::*;

    #[test]
    fn test_sse_manager_creation() {
        let manager = SseManager::new();
        assert_eq!(manager.connection_count(), 0);
    }

    #[test]
    fn test_broadcast_without_receivers() {
        let manager = SseManager::new();
        // Should not panic even with no receivers
        manager.broadcast(SseEvent::Heartbeat);
    }

    #[tokio::test]
    async fn test_broadcast_to_receiver() {
        let manager = SseManager::new();
        let mut stream = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));

        manager.broadcast(SseEvent::Status {
            message: "test".to_string(),
            thread_id: None,
        });

        let event = stream.next().await.unwrap();
        match event {
            SseEvent::Status { message, .. } => assert_eq!(message, "test"),
            _ => panic!("unexpected event type"),
        }
    }

    #[tokio::test]
    async fn test_subscribe_raw_receives_events() {
        let manager = SseManager::new();
        let mut stream = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));

        assert_eq!(manager.connection_count(), 1);

        manager.broadcast(SseEvent::Thinking {
            message: "working".to_string(),
            thread_id: None,
        });

        let event = stream.next().await.unwrap();
        match event {
            SseEvent::Thinking { message, .. } => assert_eq!(message, "working"),
            _ => panic!("Expected Thinking event"),
        }
    }

    #[tokio::test]
    async fn test_subscribe_raw_signals_lag_and_continues() {
        let manager = SseManager::new();
        let mut stream = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));

        for _ in 0..257 {
            manager.broadcast(SseEvent::Heartbeat);
        }

        assert!(matches!(
            stream.next().await,
            Some(SseEvent::StreamGap { missed_events: 1 })
        ));
        assert!(matches!(stream.next().await, Some(SseEvent::Heartbeat)));
    }

    #[tokio::test]
    async fn test_subscribe_raw_decrements_on_drop() {
        let manager = SseManager::new();
        {
            let _stream = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));
            assert_eq!(manager.connection_count(), 1);
        }
        // Stream dropped, counter should decrement
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_subscribe_raw_multiple_subscribers() {
        let manager = SseManager::new();
        let mut s1 = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));
        let mut s2 = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));
        assert_eq!(manager.connection_count(), 2);

        manager.broadcast(SseEvent::Heartbeat);

        let e1 = s1.next().await.unwrap();
        let e2 = s2.next().await.unwrap();
        assert!(matches!(e1, SseEvent::Heartbeat));
        assert!(matches!(e2, SseEvent::Heartbeat));

        drop(s1);
        assert_eq!(manager.connection_count(), 1);
        drop(s2);
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_subscribe_raw_rejects_over_limit() {
        let mut manager = SseManager::new();
        manager.max_connections = 2; // Low limit for testing

        let _s1 = Box::pin(manager.subscribe_raw(None).expect("first should succeed"));
        let _s2 = Box::pin(manager.subscribe_raw(None).expect("second should succeed"));
        assert_eq!(manager.connection_count(), 2);

        // Third should be rejected
        assert!(manager.subscribe_raw(None).is_none());
        assert!(manager.subscribe(None).is_none());
        assert!(manager.subscribe_raw_correlated(None).is_none());
        assert!(
            manager
                .subscribe_until(None, std::future::pending())
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_scoped_events_filtered_by_user() {
        let manager = SseManager::new();
        let mut alice = Box::pin(
            manager
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );
        let mut bob = Box::pin(
            manager
                .subscribe_raw(Some("bob".to_string()))
                .expect("subscribe"),
        );

        // Send event scoped to alice
        manager.broadcast_for_user(
            "alice",
            SseEvent::Status {
                message: "alice only".to_string(),
                thread_id: None,
            },
        );

        // Send global event
        manager.broadcast(SseEvent::Heartbeat);

        // Alice gets her scoped event
        let e = alice.next().await.unwrap();
        assert!(matches!(e, SseEvent::Status { .. }));

        // Alice also gets the global heartbeat
        let e = alice.next().await.unwrap();
        assert!(matches!(e, SseEvent::Heartbeat));

        // Bob only gets the global heartbeat (alice's event was filtered)
        let e = bob.next().await.unwrap(); // safety: test-only
        assert!(matches!(e, SseEvent::Heartbeat)); // safety: test assertion
    }

    // ---- Correlation (TUI design 8.3.1) ----

    const MESSAGE_ID: &str = "0b8a6c1e-5d2f-4c3b-9a1e-7f6d5c4b3a29";
    const CLIENT_TURN_ID: &str = "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e";

    fn gateway_metadata() -> serde_json::Value {
        serde_json::json!({
            "user_id": "alice",
            "thread_id": "t-1",
            "message_id": MESSAGE_ID,
            "client_turn_id": CLIENT_TURN_ID,
        })
    }

    fn correlation() -> Correlation {
        Correlation {
            message_id: MESSAGE_ID.to_string(),
            client_turn_id: Some(CLIENT_TURN_ID.to_string()),
        }
    }

    /// The next body chunk of an SSE response (one frame per chunk).
    async fn next_frame(body: &mut axum::body::BodyDataStream) -> Option<String> {
        let chunk = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("SSE stream should produce or end in time")?;
        let bytes = chunk.expect("SSE body chunk");
        Some(String::from_utf8(bytes.to_vec()).expect("UTF-8 SSE frame"))
    }

    #[test]
    fn correlated_broadcast_merges_message_ids_into_sse_payload() {
        let event = SseEvent::Thinking {
            message: "Processing...".to_string(),
            thread_id: Some("t-1".to_string()),
        };
        let payload = sse_payload(&event, Some(&correlation())).expect("payload");
        assert_eq!(
            payload,
            format!(
                r#"{{"type":"thinking","message":"Processing...","thread_id":"t-1","message_id":"{MESSAGE_ID}","client_turn_id":"{CLIENT_TURN_ID}"}}"#
            )
        );

        // Without a client id only message_id is added; unit variants work too.
        let without_client = Correlation {
            client_turn_id: None,
            ..correlation()
        };
        let payload = sse_payload(&SseEvent::Heartbeat, Some(&without_client)).expect("payload");
        assert_eq!(
            payload,
            format!(r#"{{"type":"heartbeat","message_id":"{MESSAGE_ID}"}}"#)
        );
    }

    #[test]
    fn uncorrelated_event_payload_has_no_message_id() {
        let event = SseEvent::Response {
            content: "hello".to_string(),
            thread_id: "t-1".to_string(),
            emotes: Vec::new(),
        };
        let payload = sse_payload(&event, None).expect("payload");
        assert_eq!(payload, serde_json::to_string(&event).expect("json"));
        assert!(!payload.contains("message_id"));
        assert!(!payload.contains("client_turn_id"));
    }

    #[tokio::test]
    async fn subscribe_raw_correlated_yields_correlation() {
        let manager = SseManager::new();
        let mut correlated = Box::pin(
            manager
                .subscribe_raw_correlated(Some("alice".to_string()))
                .expect("subscribe"),
        );
        let mut raw = Box::pin(
            manager
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );

        let started = SseEvent::TurnStarted {
            thread_id: "t-1".to_string(),
        };
        manager.broadcast_correlated(Some("alice"), &gateway_metadata(), started);
        manager.broadcast_correlated(Some("alice"), &serde_json::Value::Null, SseEvent::Heartbeat);
        manager.broadcast_correlated(Some("bob"), &gateway_metadata(), SseEvent::Heartbeat);
        manager.broadcast_correlated(None, &gateway_metadata(), SseEvent::Heartbeat);

        let first = correlated.next().await.expect("event");
        assert!(matches!(first.event, SseEvent::TurnStarted { .. }));
        assert_eq!(first.correlation, Some(correlation()));
        let second = correlated.next().await.expect("event");
        assert!(matches!(second.event, SseEvent::Heartbeat));
        assert_eq!(second.correlation, None, "metadata without ids");
        // Bob's event is filtered; the global one (user_id None) reaches alice.
        let third = correlated.next().await.expect("event");
        assert_eq!(third.correlation, Some(correlation()));

        // The WebSocket view of the same broadcasts carries no ids.
        assert!(matches!(
            raw.next().await,
            Some(SseEvent::TurnStarted { .. })
        ));
    }

    #[test]
    fn correlation_from_metadata_rejects_bad_ids() {
        assert_eq!(
            Correlation::from_metadata(&gateway_metadata()),
            Some(correlation())
        );
        // A UUID in another notation is accepted and rendered canonically.
        let upper = serde_json::json!({"message_id": MESSAGE_ID.to_uppercase()});
        assert_eq!(
            Correlation::from_metadata(&upper).map(|c| c.message_id),
            Some(MESSAGE_ID.to_string())
        );

        for meta in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"client_turn_id": CLIENT_TURN_ID}),
            serde_json::json!({"message_id": "not-a-uuid"}),
            serde_json::json!({"message_id": 42}),
            serde_json::json!({"message_id": ""}),
        ] {
            assert_eq!(Correlation::from_metadata(&meta), None, "{meta}");
        }

        // An invalid client id is dropped; the server id stays.
        let too_long = "a".repeat(65);
        for bad in [
            serde_json::json!("a.b"),
            serde_json::json!("a b"),
            serde_json::json!(""),
            serde_json::json!(too_long),
            serde_json::json!("line\nbreak"),
            serde_json::json!(7),
        ] {
            let meta = serde_json::json!({"message_id": MESSAGE_ID, "client_turn_id": bad});
            assert_eq!(
                Correlation::from_metadata(&meta),
                Some(Correlation {
                    message_id: MESSAGE_ID.to_string(),
                    client_turn_id: None,
                }),
                "{meta}"
            );
        }
    }

    #[tokio::test]
    async fn subscribe_until_ends_stream() {
        let manager = SseManager::new();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let sse = manager
            .subscribe_until(Some("alice".to_string()), async move {
                let _ = stopped.await;
            })
            .expect("subscribe");
        assert_eq!(manager.connection_count(), 1);

        let mut body = sse.into_response().into_body().into_data_stream();
        manager.broadcast_correlated(
            Some("alice"),
            &gateway_metadata(),
            SseEvent::TurnStarted {
                thread_id: "t-1".to_string(),
            },
        );
        let frame = next_frame(&mut body).await.expect("a frame before `until`");
        assert!(frame.starts_with("event: turn_started\n"), "{frame}");
        assert!(frame.contains(&format!(r#""client_turn_id":"{CLIENT_TURN_ID}""#)));

        // Resolving `until` ends the stream although another event is queued.
        manager.broadcast(SseEvent::Heartbeat);
        let _ = stop.send(());
        assert_eq!(
            next_frame(&mut body).await,
            None,
            "stream must end once `until` resolves"
        );
        drop(body);
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn subscribe_frames_carry_event_name_and_correlated_payload() {
        let manager = SseManager::new();
        let sse = manager.subscribe(None).expect("subscribe");
        let mut body = sse.into_response().into_body().into_data_stream();
        manager.broadcast_correlated(
            Some("alice"),
            &gateway_metadata(),
            SseEvent::TurnFinished {
                thread_id: "t-1".to_string(),
                outcome: lunarwing_common::TurnOutcome::Replied,
            },
        );
        manager.broadcast(SseEvent::Heartbeat);

        assert_eq!(
            next_frame(&mut body).await.as_deref(),
            Some(
                format!(
                    "event: turn_finished\ndata: {{\"type\":\"turn_finished\",\"thread_id\":\"t-1\",\
                     \"outcome\":\"replied\",\"message_id\":\"{MESSAGE_ID}\",\
                     \"client_turn_id\":\"{CLIENT_TURN_ID}\"}}\n\n"
                )
                .as_str()
            )
        );
        assert_eq!(
            next_frame(&mut body).await.as_deref(),
            Some("event: heartbeat\ndata: {\"type\":\"heartbeat\"}\n\n")
        );
    }
}
