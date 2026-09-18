//! Shared response policy and channel delivery for foreground and late turns.

use std::sync::Arc;

use crate::channels::{ChannelManager, IncomingMessage, OutgoingResponse};
use crate::hooks::{HookEvent, HookOutcome, HookRegistry};

/// Owned handles let a late turn use the normal delivery path without retaining
/// the whole Agent or a global engine-state lock while it waits.
#[derive(Clone)]
pub(crate) struct ResponseDelivery {
    pub(crate) channels: Arc<ChannelManager>,
    pub(crate) hooks: Arc<HookRegistry>,
    pub(crate) owner_id: String,
    pub(crate) coalesce: Option<crate::channels::coalesce::CoalesceBuffer>,
}

impl ResponseDelivery {
    pub(crate) async fn transform(
        &self,
        message: &IncomingMessage,
        thread_id: &str,
        response: String,
    ) -> Option<String> {
        let event = HookEvent::ResponseTransform {
            user_id: message.user_id.clone(),
            thread_id: thread_id.to_string(),
            response: response.clone(),
            channel: Some(message.channel.clone()),
            policy_scope: crate::channels::routing_target_from_metadata(&message.metadata),
        };
        self.apply_hooks(&event, response).await
    }

    async fn apply_hooks(&self, event: &HookEvent, original: String) -> Option<String> {
        match self.hooks.run(event).await {
            Err(error) => {
                tracing::warn!(%error, "Response hook suppressed output");
                None
            }
            Ok(HookOutcome::Reject { .. }) => None,
            Ok(HookOutcome::Continue { modified }) => Some(modified.unwrap_or(original)),
        }
    }

    pub(crate) async fn deliver(&self, message: &IncomingMessage, response: String) {
        if response.is_empty() {
            return;
        }
        let event = HookEvent::Outbound {
            user_id: message.user_id.clone(),
            channel: message.channel.clone(),
            content: response.clone(),
            thread_id: message.thread_id.clone(),
            policy_scope: crate::channels::routing_target_from_metadata(&message.metadata),
        };
        let Some(text) = self
            .apply_hooks(&event, response)
            .await
            .filter(|s| !s.is_empty())
        else {
            return;
        };

        // Preserve the normal hooks -> coalesce -> transport ordering.
        if let Some(buffer) = &self.coalesce {
            let destination = crate::channels::routing_target_from_metadata(&message.metadata)
                .unwrap_or_else(|| message.channel.clone());
            let key = crate::channels::coalesce::ChannelKey::new(
                self.owner_id.clone(),
                message.channel.clone(),
                destination,
                message.conversation_scope().unwrap_or_default().to_string(),
            );
            let channels = Arc::clone(&self.channels);
            let msg = super::agent_loop::coalesced_reply_context(message);
            let flush: crate::channels::coalesce::FlushFn = Arc::new(move |content| {
                let channels = Arc::clone(&channels);
                let msg = msg.clone();
                Box::pin(async move { respond_and_log(&channels, &msg, content).await })
            });
            match buffer.submit(key, text, flush).await {
                crate::channels::coalesce::CoalesceDecision::SendNow(content) => {
                    respond_and_log(&self.channels, message, content).await;
                }
                crate::channels::coalesce::CoalesceDecision::Buffered => {}
            }
        } else {
            respond_and_log(&self.channels, message, text).await;
        }
    }
}

async fn respond_and_log(channels: &ChannelManager, message: &IncomingMessage, content: String) {
    if let Err(error) = channels
        .respond(message, OutgoingResponse::text(content))
        .await
    {
        tracing::error!(channel = %message.channel, %error, "Failed to send response to channel");
    }
}
