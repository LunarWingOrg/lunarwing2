//! Channel manager for coordinating multiple input channels.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::stream;
use tokio::sync::{RwLock, mpsc};

use crate::channels::{
    Channel, IncomingMessage, MessageStream, OutgoingResponse, StatusUpdate,
    routing_target_from_metadata,
};
use crate::error::ChannelError;

const COORD_ENABLED_ENV: &str = "COORD_ENABLED";
const COORD_CHANNELS_ENV: &str = "COORD_CHANNELS";
const COORD_XMPP_STATUS_UPDATES_ENV: &str = "COORD_XMPP_STATUS_UPDATES";
const COORD_KNOWLEDGE_PUBLISH_ENV: &str = "COORD_KNOWLEDGE_PUBLISH";

#[derive(Debug, Clone, Default)]
struct CoordinationStatusPolicy {
    enabled: bool,
    xmpp_status_updates: bool,
    knowledge_publish_enabled: bool,
    coordination_channels: Vec<String>,
}

impl CoordinationStatusPolicy {
    fn from_env() -> Self {
        Self {
            enabled: env_flag(COORD_ENABLED_ENV),
            xmpp_status_updates: env_flag(COORD_XMPP_STATUS_UPDATES_ENV),
            knowledge_publish_enabled: env_flag(COORD_KNOWLEDGE_PUBLISH_ENV),
            coordination_channels: std::env::var(COORD_CHANNELS_ENV)
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn suppresses(
        &self,
        channel_name: &str,
        status: &StatusUpdate,
        metadata: &serde_json::Value,
    ) -> bool {
        if !self.enabled
            || self.xmpp_status_updates
            || !channel_name.eq_ignore_ascii_case("xmpp")
            || !is_transient_progress(status)
        {
            return false;
        }

        let Some(scope) = routing_target_from_metadata(metadata) else {
            return false;
        };
        self.coordination_channels
            .iter()
            .any(|configured| configured.eq_ignore_ascii_case(&scope))
    }

    fn blocks_knowledge_publish(&self, target: &str, content: &str) -> bool {
        self.enabled
            && !self.knowledge_publish_enabled
            && self
                .coordination_channels
                .iter()
                .any(|configured| configured.eq_ignore_ascii_case(target))
            && crate::hooks::coordination::is_knowledge_share_record(content)
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn is_transient_progress(status: &StatusUpdate) -> bool {
    matches!(
        status,
        StatusUpdate::Thinking(_)
            | StatusUpdate::ToolStarted { .. }
            | StatusUpdate::ToolCompleted { .. }
            | StatusUpdate::ToolResult { .. }
            | StatusUpdate::StreamChunk(_)
            | StatusUpdate::ReasoningUpdate { .. }
    )
}

fn knowledge_publish_blocked(channel_name: &str) -> ChannelError {
    ChannelError::SendFailed {
        name: channel_name.to_string(),
        reason: "KNOW publishing is disabled; set COORD_KNOWLEDGE_PUBLISH=true only after confirming fleet compatibility"
            .to_string(),
    }
}

/// Manages multiple input channels and merges their message streams.
///
/// Includes an injection channel so background tasks (e.g., job monitors) can
/// push messages into the agent loop without being a full `Channel` impl.
pub struct ChannelManager {
    channels: Arc<RwLock<HashMap<String, Arc<dyn Channel>>>>,
    /// Channels whose latest startup attempt failed while they remain registered.
    start_failures: RwLock<HashSet<String>>,
    inject_tx: mpsc::Sender<IncomingMessage>,
    /// Taken once in `start_all()` and merged into the stream.
    inject_rx: tokio::sync::Mutex<Option<mpsc::Receiver<IncomingMessage>>>,
    coordination_status_policy: CoordinationStatusPolicy,
}

impl ChannelManager {
    /// Create a new channel manager.
    pub fn new() -> Self {
        Self::with_coordination_status_policy(CoordinationStatusPolicy::from_env())
    }

    fn with_coordination_status_policy(
        coordination_status_policy: CoordinationStatusPolicy,
    ) -> Self {
        let (inject_tx, inject_rx) = mpsc::channel(64);
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            start_failures: RwLock::new(HashSet::new()),
            inject_tx,
            inject_rx: tokio::sync::Mutex::new(Some(inject_rx)),
            coordination_status_policy,
        }
    }

    /// Get a clone of the injection sender.
    ///
    /// Background tasks (like job monitors) use this to push messages into the
    /// agent loop without being a full `Channel` implementation.
    pub fn inject_sender(&self) -> mpsc::Sender<IncomingMessage> {
        self.inject_tx.clone()
    }

    /// Add a channel to the manager.
    pub async fn add(&self, channel: Box<dyn Channel>) {
        let name = channel.name().to_string();
        self.start_failures.write().await.remove(&name);
        self.channels
            .write()
            .await
            .insert(name.clone(), Arc::from(channel));
        tracing::debug!("Added channel: {}", name);
    }

    /// Hot-add a channel to a running agent.
    ///
    /// Starts the channel, registers it in the channels map for `respond()`/`broadcast()`,
    /// and spawns a task that forwards its stream messages through `inject_tx` into
    /// the agent loop.
    pub async fn hot_add(&self, channel: Box<dyn Channel>) -> Result<(), ChannelError> {
        let name = channel.name().to_string();

        // Shut down any existing channel with the same name to avoid parallel consumers.
        // The old forwarding task will stop when the channel's stream ends after shutdown.
        {
            let channels = self.channels.read().await;
            if let Some(existing) = channels.get(&name) {
                tracing::debug!(channel = %name, "Shutting down existing channel before hot-add replacement");
                let _ = existing.shutdown().await;
            }
        }

        let stream = match channel.start().await {
            Ok(stream) => {
                self.start_failures.write().await.remove(&name);
                stream
            }
            Err(error) => {
                self.start_failures.write().await.insert(name);
                return Err(error);
            }
        };

        // Register for respond/broadcast/send_status
        self.channels
            .write()
            .await
            .insert(name.clone(), Arc::from(channel));

        // Forward stream messages through inject_tx
        let tx = self.inject_tx.clone();
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = stream;
            while let Some(msg) = stream.next().await {
                if tx.send(msg).await.is_err() {
                    tracing::warn!(channel = %name, "Inject channel closed, stopping hot-added channel");
                    break;
                }
            }
            tracing::debug!(channel = %name, "Hot-added channel stream ended");
        });

        Ok(())
    }

    /// Start all channels and return a merged stream of messages.
    ///
    /// Also merges the injection channel so background tasks can push messages
    /// into the same stream.
    pub async fn start_all(&self) -> Result<MessageStream, ChannelError> {
        let channels = self.channels.read().await;
        let mut streams: Vec<MessageStream> = Vec::new();

        for (name, channel) in channels.iter() {
            match channel.start().await {
                Ok(stream) => {
                    self.start_failures.write().await.remove(name);
                    tracing::debug!("Started channel: {}", name);
                    streams.push(stream);
                }
                Err(error) => {
                    self.start_failures.write().await.insert(name.clone());
                    tracing::error!("Failed to start channel {}: {}", name, error);
                    // Continue with other channels, don't fail completely
                }
            }
        }

        if streams.is_empty() {
            return Err(ChannelError::StartupFailed {
                name: "all".to_string(),
                reason: "No channels started successfully".to_string(),
            });
        }

        // Take the injection receiver (can only be taken once)
        if let Some(inject_rx) = self.inject_rx.lock().await.take() {
            let inject_stream = tokio_stream::wrappers::ReceiverStream::new(inject_rx);
            streams.push(Box::pin(inject_stream));
            tracing::debug!("Injection channel merged into message stream");
        }

        // Merge all streams into one
        let merged = stream::select_all(streams);
        Ok(Box::pin(merged))
    }

    /// Send a response to a specific channel.
    pub async fn respond(
        &self,
        msg: &IncomingMessage,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        let target = msg.routing_target().unwrap_or_else(|| msg.user_id.clone());
        if self
            .coordination_status_policy
            .blocks_knowledge_publish(&target, &response.content)
        {
            return Err(knowledge_publish_blocked(&msg.channel));
        }
        let channels = self.channels.read().await;
        if let Some(channel) = channels.get(&msg.channel) {
            channel.respond(msg, response).await
        } else {
            Err(ChannelError::SendFailed {
                name: msg.channel.clone(),
                reason: "Channel not found".to_string(),
            })
        }
    }

    /// Send a status update to a specific channel.
    ///
    /// The metadata contains channel-specific routing info (e.g., XMPP chat_id)
    /// needed to deliver the status to the correct destination.
    pub async fn send_status(
        &self,
        channel_name: &str,
        status: StatusUpdate,
        metadata: &serde_json::Value,
    ) -> Result<(), ChannelError> {
        if self
            .coordination_status_policy
            .suppresses(channel_name, &status, metadata)
        {
            tracing::debug!(
                channel = channel_name,
                scope = ?routing_target_from_metadata(metadata),
                "Suppressed transient status in coordination channel"
            );
            return Ok(());
        }

        let channels = self.channels.read().await;
        if let Some(channel) = channels.get(channel_name) {
            channel.send_status(status, metadata).await
        } else {
            // Silently ignore if channel not found (status is best-effort)
            Ok(())
        }
    }

    /// Broadcast a message to a specific user on a specific channel.
    ///
    /// Used for proactive notifications like heartbeat alerts.
    pub async fn broadcast(
        &self,
        channel_name: &str,
        user_id: &str,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        if self
            .coordination_status_policy
            .blocks_knowledge_publish(user_id, &response.content)
        {
            return Err(knowledge_publish_blocked(channel_name));
        }
        let channels = self.channels.read().await;
        if let Some(channel) = channels.get(channel_name) {
            channel.broadcast(user_id, response).await
        } else {
            Err(ChannelError::SendFailed {
                name: channel_name.to_string(),
                reason: "Channel not found".to_string(),
            })
        }
    }

    /// Broadcast a message to all channels.
    ///
    /// Sends to the specified user on every registered channel.
    pub async fn broadcast_all(
        &self,
        user_id: &str,
        response: OutgoingResponse,
    ) -> Vec<(String, Result<(), ChannelError>)> {
        let channels = self.channels.read().await;
        let mut results = Vec::new();

        for (name, channel) in channels.iter() {
            let result = if self
                .coordination_status_policy
                .blocks_knowledge_publish(user_id, &response.content)
            {
                Err(knowledge_publish_blocked(name))
            } else {
                channel.broadcast(user_id, response.clone()).await
            };
            results.push((name.clone(), result));
        }

        results
    }

    /// Check health of all channels.
    pub async fn health_check_all(&self) -> HashMap<String, Result<(), ChannelError>> {
        let channels = self.channels.read().await;
        let start_failures = self.start_failures.read().await.clone();
        let mut results = HashMap::new();

        for (name, channel) in channels.iter() {
            let result = if start_failures.contains(name) {
                Err(ChannelError::HealthCheckFailed {
                    name: name.clone(),
                    reason: "channel failed to start; see daemon logs".to_string(),
                })
            } else {
                channel.health_check().await
            };
            results.insert(name.clone(), result);
        }

        results
    }

    /// Shutdown all channels.
    pub async fn shutdown_all(&self) -> Result<(), ChannelError> {
        let channels = self.channels.read().await;
        for (name, channel) in channels.iter() {
            if let Err(e) = channel.shutdown().await {
                tracing::error!("Error shutting down channel {}: {}", name, e);
            }
        }
        Ok(())
    }

    /// Get list of channel names.
    pub async fn channel_names(&self) -> Vec<String> {
        self.channels.read().await.keys().cloned().collect()
    }

    /// Get a channel by name.
    pub async fn get_channel(&self, name: &str) -> Option<Arc<dyn Channel>> {
        self.channels.read().await.get(name).cloned()
    }

    /// Remove a channel from the manager.
    pub async fn remove(&self, name: &str) -> Option<Arc<dyn Channel>> {
        self.start_failures.write().await.remove(name);
        self.channels.write().await.remove(name)
    }
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::IncomingMessage;
    use crate::testing::StubChannel;
    use futures::StreamExt;

    fn coordination_policy(xmpp_status_updates: bool) -> CoordinationStatusPolicy {
        CoordinationStatusPolicy {
            enabled: true,
            xmpp_status_updates,
            knowledge_publish_enabled: false,
            coordination_channels: vec!["ops@conference.example".to_string()],
        }
    }

    #[tokio::test]
    async fn coordination_xmpp_suppresses_transient_status_only_in_configured_room() {
        let manager = ChannelManager::with_coordination_status_policy(coordination_policy(false));
        let (stub, _) = StubChannel::new("xmpp");
        let statuses = stub.captured_statuses_handle();
        manager.add(Box::new(stub)).await;

        let coord_metadata = serde_json::json!({
            "xmpp_target": "OPS@conference.example",
            "xmpp_type": "groupchat"
        });
        let other_metadata = serde_json::json!({
            "xmpp_target": "general@conference.example",
            "xmpp_type": "groupchat"
        });
        let reasoning = StatusUpdate::reasoning_for_tools(&["memory_read".to_string()])
            .expect("reasoning status");

        manager
            .send_status("xmpp", reasoning.clone(), &coord_metadata)
            .await
            .expect("coordination status suppression");
        manager
            .send_status("xmpp", reasoning, &other_metadata)
            .await
            .expect("normal room status delivery");
        manager
            .send_status(
                "xmpp",
                StatusUpdate::Status("service warning".to_string()),
                &coord_metadata,
            )
            .await
            .expect("non-transient coordination status delivery");

        let captured = statuses.lock().expect("poisoned");
        assert_eq!(captured.len(), 2);
        assert!(matches!(captured[0], StatusUpdate::ReasoningUpdate { .. }));
        assert!(matches!(captured[1], StatusUpdate::Status(_)));
    }

    #[test]
    fn coordination_xmpp_status_override_restores_progress() {
        let policy = coordination_policy(true);
        let metadata = serde_json::json!({ "xmpp_target": "ops@conference.example" });
        let status = StatusUpdate::Thinking("working".to_string());
        assert!(!policy.suppresses("xmpp", &status, &metadata));
    }

    #[tokio::test]
    async fn channel_boundary_blocks_knowledge_publish_until_enabled() {
        use crate::testing::RecordingBroadcastChannel;

        let manager = ChannelManager::with_coordination_status_policy(coordination_policy(false));
        let (channel, captures) = RecordingBroadcastChannel::new("xmpp");
        manager.add(Box::new(channel)).await;
        let record = OutgoingResponse::text("KNOW: topic | finding long enough");

        assert!(
            manager
                .broadcast("xmpp", "OPS@conference.example", record.clone())
                .await
                .is_err()
        );
        assert!(captures.lock().await.is_empty());
        let results = manager
            .broadcast_all("ops@conference.example", record.clone())
            .await;
        assert!(results.iter().all(|(_, result)| result.is_err()));
        assert!(captures.lock().await.is_empty());

        let mut policy = coordination_policy(false);
        policy.knowledge_publish_enabled = true;
        let manager = ChannelManager::with_coordination_status_policy(policy);
        let (channel, captures) = RecordingBroadcastChannel::new("xmpp");
        manager.add(Box::new(channel)).await;
        manager
            .broadcast("xmpp", "ops@conference.example", record)
            .await
            .expect("enabled KNOW publish reaches channel boundary");
        assert_eq!(captures.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn response_boundary_blocks_knowledge_publish_until_enabled() {
        let manager = ChannelManager::with_coordination_status_policy(coordination_policy(false));
        let (stub, _) = StubChannel::new("xmpp");
        let responses = stub.captured_responses_handle();
        manager.add(Box::new(stub)).await;
        let message = IncomingMessage::new("xmpp", "owner", "request")
            .with_sender_id("ops@conference.example");

        assert!(
            manager
                .respond(
                    &message,
                    OutgoingResponse::text("KNOW: topic | finding long enough"),
                )
                .await
                .is_err()
        );
        assert!(responses.lock().expect("poisoned").is_empty());
    }

    #[tokio::test]
    async fn test_add_and_start_all() {
        let manager = ChannelManager::new();
        let (stub, sender) = StubChannel::new("test");

        manager.add(Box::new(stub)).await;

        let mut stream = manager.start_all().await.expect("start_all failed");

        // Inject a message through the stub
        sender
            .send(IncomingMessage::new("test", "user1", "hello"))
            .await
            .expect("send failed");

        // Should appear in the merged stream
        let msg = stream.next().await.expect("stream ended");
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.channel, "test");
    }

    #[tokio::test]
    async fn test_respond_routes_to_correct_channel() {
        let manager = ChannelManager::new();
        let (stub, _sender) = StubChannel::new("alpha");

        // Keep a reference for response inspection
        let responses = stub.captured_responses_handle();
        manager.add(Box::new(stub)).await;

        let msg = IncomingMessage::new("alpha", "user1", "request");
        manager
            .respond(&msg, OutgoingResponse::text("reply"))
            .await
            .expect("respond failed");

        // Verify the stub captured the response
        let captured = responses.lock().expect("poisoned");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].1.content, "reply");
    }

    #[tokio::test]
    async fn test_respond_unknown_channel_errors() {
        let manager = ChannelManager::new();
        let msg = IncomingMessage::new("nonexistent", "user1", "test");
        let result = manager.respond(&msg, OutgoingResponse::text("hi")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_health_check_all() {
        let manager = ChannelManager::new();
        let (stub1, _) = StubChannel::new("healthy");
        let (stub2, _) = StubChannel::new("sick");
        stub2.set_healthy(false);

        manager.add(Box::new(stub1)).await;
        manager.add(Box::new(stub2)).await;

        let results = manager.health_check_all().await;
        assert!(results["healthy"].is_ok());
        assert!(results["sick"].is_err());
    }

    #[tokio::test]
    async fn startup_failure_overrides_optimistic_channel_health_until_replaced() {
        let manager = ChannelManager::new();
        let (stub, _) = StubChannel::new("optional");
        manager.add(Box::new(stub)).await;

        let _stream = manager.start_all().await.expect("initial start");
        let second_start = manager.start_all().await;
        assert!(second_start.is_err());

        let results = manager.health_check_all().await;
        let error = results["optional"]
            .as_ref()
            .expect_err("failed startup must make channel unhealthy")
            .to_string();
        assert!(error.contains("channel failed to start"), "{error}");
        assert!(!error.contains("already called"), "{error}");

        let (replacement, _) = StubChannel::new("optional");
        manager.add(Box::new(replacement)).await;
        assert!(manager.health_check_all().await["optional"].is_ok());
    }

    #[tokio::test]
    async fn test_start_all_no_channels_errors() {
        let manager = ChannelManager::new();
        let result = manager.start_all().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_injection_channel_merges() {
        let manager = ChannelManager::new();
        let (stub, _sender) = StubChannel::new("real");
        manager.add(Box::new(stub)).await;

        let mut stream = manager.start_all().await.expect("start_all failed");

        // Use the injection channel (simulating background task)
        let inject_tx = manager.inject_sender();
        inject_tx
            .send(IncomingMessage::new(
                "injected",
                "system",
                "background alert",
            ))
            .await
            .expect("inject failed");

        let msg = stream.next().await.expect("stream ended");
        assert_eq!(msg.content, "background alert");
    }

    #[tokio::test]
    async fn test_hot_add_replaces_existing_channel() {
        // Regression: hot_add must shut down the existing channel before replacing it,
        // to prevent duplicate SSE consumers from running in parallel.
        let manager = ChannelManager::new();
        let (stub1, _tx1) = StubChannel::new("relay");
        manager.add(Box::new(stub1)).await;
        let mut stream = manager.start_all().await.expect("start_all");

        // Hot-add a replacement channel with the same name
        let (stub2, tx2) = StubChannel::new("relay");
        manager.hot_add(Box::new(stub2)).await.expect("hot_add");

        // Send through the new channel — should arrive in the merged stream
        tx2.send(IncomingMessage::new("relay", "u1", "from new"))
            .await
            .expect("send");
        let msg = stream.next().await.expect("stream");
        assert_eq!(msg.content, "from new");

        // Verify only one channel entry exists
        let channels = manager.channels.read().await;
        assert_eq!(channels.len(), 1);
        assert!(channels.contains_key("relay"));
    }
}
