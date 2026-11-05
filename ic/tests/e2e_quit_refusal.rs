//! E2E tests: `/quit`, `/exit` and `/shutdown` cannot stop the daemon from the TUI
//! channels (TUI design 8.4).
//!
//! The web gateway (`gateway`) and the Unix-socket REPL (`unix_socket_repl`) are clients
//! attached to a running service: the agent refuses the three commands from them and
//! keeps running. With decision DEC-1 and WI#550 a silent `NO_REPLY` model reply
//! does not stop the agent on those channels or non-TUI service channels such as
//! `xmpp`. The in-process REPL (`repl`) still exits. These tests drive the V1 agent
//! loop (`ENGINE_V2` unset, as in CI); a `ShutdownProbe` channel observes whether
//! the loop shut the channels down.

#[cfg(feature = "libsql")]
mod support;

#[cfg(feature = "libsql")]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use uuid::Uuid;

    use lunarwing::channels::{
        Channel, IncomingMessage, MessageLifecycle, MessageStream, OutgoingResponse, TurnOutcome,
    };
    use lunarwing::error::ChannelError;

    use crate::support::test_rig::{TestRig, TestRigBuilder};
    use crate::support::trace_llm::LlmTrace;

    const TIMEOUT: Duration = Duration::from_secs(15);
    /// Time allowed for a wrongly accepted `/quit` to shut the loop down.
    const SETTLE: Duration = Duration::from_millis(300);
    const USER: &str = "test-user";
    const GATEWAY: &str = "gateway";
    const SOCKET: &str = "unix_socket_repl";

    /// A second channel whose `shutdown()` is called when the agent loop exits.
    struct ShutdownProbe {
        stopped: Arc<AtomicBool>,
    }

    impl ShutdownProbe {
        fn new() -> (Self, Arc<AtomicBool>) {
            let stopped = Arc::new(AtomicBool::new(false));
            let probe = Self {
                stopped: Arc::clone(&stopped),
            };
            (probe, stopped)
        }
    }

    #[async_trait]
    impl Channel for ShutdownProbe {
        fn name(&self) -> &str {
            "shutdown-probe"
        }

        async fn start(&self) -> Result<MessageStream, ChannelError> {
            Ok(Box::pin(futures::stream::pending::<IncomingMessage>()))
        }

        async fn respond(
            &self,
            _msg: &IncomingMessage,
            _response: OutgoingResponse,
        ) -> Result<(), ChannelError> {
            Ok(())
        }

        async fn health_check(&self) -> Result<(), ChannelError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), ChannelError> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn fixture(name: &str) -> LlmTrace {
        let path = format!(
            "{}/tests/fixtures/llm_traces/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        LlmTrace::from_file(&path).unwrap_or_else(|e| panic!("failed to load {path}: {e}"))
    }

    /// A rig whose capture channel is named `channel`, plus the shutdown probe's flag.
    async fn rig_for(channel: &str, trace: LlmTrace) -> (TestRig, Arc<AtomicBool>) {
        let (probe, stopped) = ShutdownProbe::new();
        let rig = TestRigBuilder::new()
            .with_channel_name(channel)
            .with_trace(trace)
            .with_channel(Box::new(probe))
            .build()
            .await;
        (rig, stopped)
    }

    async fn send(rig: &TestRig, channel: &str, content: &str) -> Uuid {
        let message = IncomingMessage::new(channel, USER, content);
        let id = message.id;
        rig.send_incoming(message).await;
        id
    }

    /// Waits until message `id` reported `Finished`, then returns its events.
    async fn wait_for_finish(rig: &TestRig, id: Uuid) -> Vec<MessageLifecycle> {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let events: Vec<MessageLifecycle> = rig
                .captured_lifecycle()
                .into_iter()
                .filter(|(message_id, _)| *message_id == id)
                .map(|(_, event)| event)
                .collect();
            let done = events
                .iter()
                .any(|event| matches!(event, MessageLifecycle::Finished(_)));
            if done || tokio::time::Instant::now() >= deadline {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_until_stopped(stopped: &AtomicBool) -> bool {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !stopped.load(Ordering::SeqCst) {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        true
    }

    /// Sends `command` on `channel` as the rig's message number `index` (from 0); it must
    /// be refused with an `Error: ...` reply and finish `failed`.
    async fn assert_command_refused(rig: &TestRig, channel: &str, command: &str, index: usize) {
        let id = send(rig, channel, command).await;
        let responses = rig.wait_for_responses(index + 1, TIMEOUT).await;
        assert_eq!(responses.len(), index + 1, "{command} got no reply");
        let reply = &responses[index].content;
        assert!(
            reply.starts_with("Error: ") && reply.contains("cannot stop"),
            "{command} on {channel}: unexpected reply {reply:?}"
        );
        assert_eq!(
            wait_for_finish(rig, id).await,
            [
                MessageLifecycle::Started,
                MessageLifecycle::Finished(TurnOutcome::Failed)
            ],
            "{command} on {channel}"
        );
    }

    /// After `earlier` replies, a `hello` on `channel` is still answered with `expected`
    /// and finishes `replied`, and the channels were never shut down.
    async fn assert_still_answering(
        rig: &TestRig,
        channel: &str,
        stopped: &AtomicBool,
        earlier: usize,
        expected: &str,
    ) {
        let hello = send(rig, channel, "hello").await;
        let responses = rig.wait_for_responses(earlier + 1, TIMEOUT).await;
        assert_eq!(
            responses.len(),
            earlier + 1,
            "the agent stopped answering on {channel}"
        );
        assert_eq!(responses[earlier].content, expected);
        assert_eq!(
            wait_for_finish(rig, hello).await,
            [
                MessageLifecycle::Started,
                MessageLifecycle::Finished(TurnOutcome::Replied)
            ]
        );
        assert!(!stopped.load(Ordering::SeqCst));
    }

    /// Sends `/quit`, `/exit` and `/shutdown` on `channel`; each must be refused with a
    /// reply and finish `failed`, and a later `hello` must still be answered.
    async fn assert_quit_is_refused(channel: &str) {
        let (rig, stopped) = rig_for(channel, fixture("simple_text.json")).await;

        let commands = ["/quit", "/exit", "/shutdown"];
        for (index, command) in commands.into_iter().enumerate() {
            assert_command_refused(&rig, channel, command, index).await;
        }
        tokio::time::sleep(SETTLE).await;
        assert!(
            !stopped.load(Ordering::SeqCst),
            "a refused command shut the channels down"
        );

        let expected = "Hello from fixture file!";
        assert_still_answering(&rig, channel, &stopped, commands.len(), expected).await;

        rig.shutdown();
    }

    /// DEC-1/WI#550: a `NO_REPLY` model reply is suppressed and finishes `no_reply`,
    /// and the agent keeps answering on `channel`.
    async fn assert_silent_reply_keeps_running(channel: &str) {
        let (rig, stopped) = rig_for(channel, fixture("tui/silent_reply.json")).await;

        let mut silent_message =
            IncomingMessage::new(channel, USER, "Nothing to add here, stay quiet.");
        if channel == "xmpp" {
            silent_message = silent_message
                .with_metadata(serde_json::json!({"chat_type": "group", "xmpp_type": "groupchat"}));
        }
        let silent = silent_message.id;
        rig.send_incoming(silent_message).await;
        assert_eq!(
            wait_for_finish(&rig, silent).await,
            [
                MessageLifecycle::Started,
                MessageLifecycle::Finished(TurnOutcome::NoReply)
            ]
        );
        tokio::time::sleep(SETTLE).await;
        assert!(
            rig.captured_responses().is_empty(),
            "a silent reply must not be delivered"
        );
        assert!(
            !stopped.load(Ordering::SeqCst),
            "a silent reply shut the channels down"
        );

        let expected = "Still here after a silent reply.";
        assert_still_answering(&rig, channel, &stopped, 0, expected).await;

        rig.shutdown();
    }

    #[tokio::test]
    async fn socket_quit_is_refused_and_agent_keeps_running() {
        assert_quit_is_refused(SOCKET).await;
    }

    #[tokio::test]
    async fn gateway_quit_is_refused_and_agent_keeps_running() {
        assert_quit_is_refused(GATEWAY).await;
    }

    #[tokio::test]
    async fn socket_silent_reply_does_not_stop_agent() {
        assert_silent_reply_keeps_running(SOCKET).await;
    }

    #[tokio::test]
    async fn gateway_silent_reply_does_not_stop_agent() {
        assert_silent_reply_keeps_running(GATEWAY).await;
    }

    #[tokio::test]
    async fn xmpp_group_silent_reply_does_not_stop_agent() {
        assert_silent_reply_keeps_running("xmpp").await;
    }

    /// The in-process REPL owns the process: `/quit` still stops the agent loop.
    #[tokio::test]
    async fn local_repl_quit_still_stops_agent() {
        let (rig, stopped) = rig_for("repl", fixture("simple_text.json")).await;

        let id = send(&rig, "repl", "/quit").await;
        assert_eq!(
            wait_for_finish(&rig, id).await,
            [
                MessageLifecycle::Started,
                MessageLifecycle::Finished(TurnOutcome::NoReply)
            ]
        );
        assert!(
            wait_until_stopped(&stopped).await,
            "/quit from the in-process REPL must stop the agent"
        );
        assert!(rig.captured_responses().is_empty());

        rig.shutdown();
    }
}
