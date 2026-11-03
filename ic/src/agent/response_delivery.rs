//! Shared response policy and channel delivery for foreground and late turns.

use std::sync::Arc;

use crate::channels::{ChannelManager, IncomingMessage, OutgoingResponse, TurnOutcome};
use crate::hooks::{HookEvent, HookOutcome, HookRegistry};

/// What [`ResponseDelivery::deliver`] did with a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Handed to the channel's `respond()` (whether or not the send itself succeeded).
    Sent,
    /// Taken by the coordination coalescing buffer, which hands it to `respond()` later.
    Buffered,
    /// Never handed to `respond()`: the reply was empty, or a `BeforeOutbound` hook
    /// rejected it, failed or rewrote it to nothing.
    Suppressed,
}

impl Delivery {
    /// How a turn whose non-empty reply went this way finished (TUI design 8.2.2):
    /// `replied` when the reply is on its way to the channel, `no_reply` otherwise.
    pub(crate) fn outcome(self) -> TurnOutcome {
        match self {
            Self::Sent | Self::Buffered => TurnOutcome::Replied,
            Self::Suppressed => TurnOutcome::NoReply,
        }
    }
}

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

    /// Runs the `BeforeOutbound` hooks on `response`, then coalesces or sends it, and
    /// reports which of the two happened, or that nothing was sent.
    pub(crate) async fn deliver(&self, message: &IncomingMessage, response: String) -> Delivery {
        if response.is_empty() {
            return Delivery::Suppressed;
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
            return Delivery::Suppressed;
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
                    Delivery::Sent
                }
                crate::channels::coalesce::CoalesceDecision::Buffered => Delivery::Buffered,
            }
        } else {
            respond_and_log(&self.channels, message, text).await;
            Delivery::Sent
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::{Delivery, ResponseDelivery};
    use crate::channels::{ChannelManager, IncomingMessage, OutgoingResponse, TurnOutcome};
    use crate::hooks::{
        Hook, HookContext, HookError, HookEvent, HookFailureMode, HookOutcome, HookPoint,
        HookRegistry,
    };
    use crate::testing::StubChannel;

    type Replies = Arc<Mutex<Vec<(IncomingMessage, OutgoingResponse)>>>;

    #[derive(Clone, Copy, Debug)]
    enum Outbound {
        Reject,
        Empty,
        Fail,
    }

    struct OutboundHook(Outbound);

    #[async_trait]
    impl Hook for OutboundHook {
        fn name(&self) -> &str {
            "delivery-outbound-test"
        }

        fn hook_points(&self) -> &[HookPoint] {
            &[HookPoint::BeforeOutbound]
        }

        fn failure_mode(&self) -> HookFailureMode {
            HookFailureMode::FailClosed
        }

        async fn execute(
            &self,
            _event: &HookEvent,
            _ctx: &HookContext,
        ) -> Result<HookOutcome, HookError> {
            match self.0 {
                Outbound::Reject => Ok(HookOutcome::reject("blocked by the test")),
                Outbound::Empty => Ok(HookOutcome::modify(String::new())),
                Outbound::Fail => Err(HookError::ExecutionFailed {
                    reason: "hook backend down".into(),
                }),
            }
        }
    }

    async fn delivery(hook: Option<Outbound>) -> (ResponseDelivery, Replies) {
        let channels = Arc::new(ChannelManager::new());
        let (channel, _sender) = StubChannel::new("stub");
        let replies = channel.captured_responses_handle();
        channels.add(Box::new(channel)).await;
        let hooks = Arc::new(HookRegistry::default());
        if let Some(behavior) = hook {
            hooks.register(Arc::new(OutboundHook(behavior))).await;
        }
        let delivery = ResponseDelivery {
            channels,
            hooks,
            owner_id: "alice".into(),
            coalesce: None,
        };
        (delivery, replies)
    }

    fn reply_count(replies: &Replies) -> usize {
        replies.lock().map_or(0, |replies| replies.len())
    }

    #[tokio::test]
    async fn deliver_reports_whether_the_reply_reached_respond() {
        let message = IncomingMessage::new("stub", "alice", "hello");
        let (plain, replies) = delivery(None).await;
        let sent = plain.deliver(&message, "hi there".into()).await;
        assert_eq!(sent, Delivery::Sent); // safety: test-only assertion
        let empty = plain.deliver(&message, String::new()).await;
        assert_eq!(empty, Delivery::Suppressed); // safety: test-only assertion
        assert_eq!(reply_count(&replies), 1); // safety: test-only assertion

        // A rejecting, emptying or failing (fail-closed) outbound hook stops the reply
        // before `respond()`, and the delivery says so.
        for behavior in [Outbound::Reject, Outbound::Empty, Outbound::Fail] {
            let (hooked, replies) = delivery(Some(behavior)).await;
            let stopped = hooked.deliver(&message, "hi there".into()).await;
            assert_eq!(stopped, Delivery::Suppressed, "{behavior:?}"); // safety: test-only assertion
            assert_eq!(reply_count(&replies), 0, "{behavior:?}"); // safety: test-only assertion
        }
    }

    #[test]
    fn delivery_outcome_is_replied_only_when_the_reply_is_on_its_way() {
        let outcomes = [
            Delivery::Sent.outcome(),
            Delivery::Buffered.outcome(),
            Delivery::Suppressed.outcome(),
        ];
        let expected = [
            TurnOutcome::Replied,
            TurnOutcome::Replied,
            TurnOutcome::NoReply,
        ];
        assert_eq!(outcomes, expected); // safety: test-only assertion
    }
}
