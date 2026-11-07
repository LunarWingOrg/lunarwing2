use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use futures::StreamExt;
use lunarwing_common::TurnOutcome;
use lunarwing_common::repl_wire::{
    ClientFrame, ConnectFrame, MessageFrame, ResponseFrame, ServerFrame, WireAttachment,
    encode_client_line,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

use super::listener::{Existing, ReplSocketError, bind_socket, classify_existing, peer_allowed};
use super::*;

fn test_channel(path: &Path, max_connections: usize) -> UnixSocketReplChannel {
    UnixSocketReplChannel::new(path, max_connections, "test-agent", "test-owner")
}

fn fast_limits() -> ReplLimits {
    ReplLimits {
        handshake_timeout: Duration::from_millis(80),
        write_timeout: Duration::from_millis(80),
        outbound_queue_frames: 8,
        outbound_queue_bytes: 128 * 1024,
        status_shed_bytes: 1024,
        ..ReplLimits::default()
    }
}

async fn start(channel: &UnixSocketReplChannel) -> MessageStream {
    channel.start().await.expect("channel start")
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("path metadata")
        .permissions()
        .mode()
        & 0o7777
}

struct TestClient {
    reader: BufReader<UnixStream>,
}

impl TestClient {
    async fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).await.expect("connect REPL");
        Self {
            reader: BufReader::new(stream),
        }
    }

    async fn send(&mut self, frame: &ClientFrame) {
        let bytes = encode_client_line(frame).expect("client frame encoding");
        self.send_raw(&bytes).await;
    }

    async fn send_raw(&mut self, bytes: &[u8]) {
        self.reader
            .get_mut()
            .write_all(bytes)
            .await
            .expect("write client frame");
    }

    async fn read_optional(&mut self) -> Option<ServerFrame> {
        let mut line = String::new();
        let read = timeout(Duration::from_secs(2), self.reader.read_line(&mut line))
            .await
            .expect("server read timeout")
            .expect("server read");
        if read == 0 {
            return None;
        }
        Some(serde_json::from_str(&line).expect("server frame"))
    }

    async fn read(&mut self) -> ServerFrame {
        self.read_optional()
            .await
            .expect("server closed connection")
    }

    async fn handshake(path: &Path) -> (Self, String) {
        let mut client = Self::connect(path).await;
        client.send(&legacy_connect()).await;
        let frame = client.read().await;
        let text = response_text(&frame);
        let session_id = match frame {
            ServerFrame::Response(ResponseFrame { session_id, .. }) => {
                session_id.expect("welcome session id")
            }
            other => panic!("expected legacy welcome, got {}", other.kind()),
        };
        assert!(text.contains("Connected to test-agent REPL"), "{text}");
        (client, session_id)
    }
}

fn legacy_connect() -> ClientFrame {
    ClientFrame::Connect(ConnectFrame {
        version: "2.1.9".to_string(),
        client_info: None,
        protocol: None,
        features: Vec::new(),
        thread: None,
    })
}

fn response_text(frame: &ServerFrame) -> String {
    match frame {
        ServerFrame::Response(response) => response.content.clone(),
        other => panic!("expected Response, got {}", other.kind()),
    }
}

fn incoming(session_id: &str, content: &str) -> IncomingMessage {
    IncomingMessage::new(
        crate::channels::UNIX_SOCKET_REPL_CHANNEL,
        "test-owner",
        content,
    )
    .with_sender_id(session_id.to_string())
    .with_metadata(serde_json::json!({"unix_session_id": session_id}))
}

#[tokio::test]
async fn socket_file_mode_is_0600() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    assert_eq!(mode(&path), 0o600);
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn shutdown_unlinks_socket_and_channel_can_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let stream = start(&channel).await;
    drop(stream);
    channel.shutdown().await.expect("first shutdown");
    assert!(!path.exists());

    let _stream = start(&channel).await;
    let (_client, _session) = TestClient::handshake(&path).await;
    channel.shutdown().await.expect("second shutdown");
    assert!(!path.exists());
}

#[tokio::test]
async fn missing_parent_dir_is_created_0700() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("nested/run");
    let path = parent.join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    assert_eq!(mode(&parent), 0o700);
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn existing_parent_dir_mode_is_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("run");
    std::fs::create_dir(&parent).expect("parent");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).expect("parent mode");
    let path = parent.join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    assert_eq!(mode(&parent), 0o750);
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn group_writable_parent_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("run");
    std::fs::create_dir(&parent).expect("parent");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o770)).expect("parent mode");
    let channel = test_channel(&parent.join("repl.sock"), 2);
    let _stream = start(&channel).await;
    assert!(matches!(
        &*channel.state(),
        ListenerState::NotServing { .. }
    ));
    assert!(channel.health_check().await.is_ok());
    assert_eq!(mode(&parent), 0o770);
}

#[tokio::test]
async fn world_writable_sticky_parent_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("run");
    std::fs::create_dir(&parent).expect("parent");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o1777))
        .expect("parent mode");
    let channel = test_channel(&parent.join("repl.sock"), 2);
    let _stream = start(&channel).await;
    assert!(matches!(
        &*channel.state(),
        ListenerState::NotServing { .. }
    ));
    assert!(channel.health_check().await.is_ok());
    assert_eq!(mode(&parent), 0o1777);
}

#[tokio::test]
async fn foreign_uid_peer_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let expected = nix::unistd::geteuid().as_raw().wrapping_add(1);
    let channel = test_channel(&path, 2).with_expected_uid(expected);
    let _stream = start(&channel).await;
    let mut client = TestClient::connect(&path).await;
    assert!(client.read_optional().await.is_none());
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn same_uid_peer_is_accepted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (_client, _session) = TestClient::handshake(&path).await;
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn peer_allowed_rejects_mismatched_uid() {
    let (stream, _peer) = UnixStream::pair().expect("stream pair");
    let expected = nix::unistd::geteuid().as_raw().wrapping_add(1);
    assert!(peer_allowed(&stream, expected).is_err());
}

#[tokio::test]
async fn start_refuses_to_steal_live_socket() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let first = test_channel(&path, 2);
    let _first_stream = start(&first).await;
    let second = test_channel(&path, 2);
    let _second_stream = start(&second).await;
    assert!(matches!(&*second.state(), ListenerState::NotServing { .. }));
    let (_client, _session) = TestClient::handshake(&path).await;
    second.shutdown().await.expect("second shutdown");
    assert!(path.exists());
    first.shutdown().await.expect("first shutdown");
}

#[tokio::test]
async fn start_replaces_stale_socket() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).expect("stale socket"));
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (_client, _session) = TestClient::handshake(&path).await;
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn start_refuses_regular_file_at_socket_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    std::fs::write(&path, b"do not remove").expect("regular file");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    assert!(matches!(
        &*channel.state(),
        ListenerState::NotServing { .. }
    ));
    assert_eq!(std::fs::read(&path).expect("file"), b"do not remove");
}

#[tokio::test]
async fn start_refuses_symlink_at_socket_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::write(&target, b"keep").expect("target");
    let path = temp.path().join("repl.sock");
    std::os::unix::fs::symlink(&target, &path).expect("symlink");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    assert!(matches!(
        &*channel.state(),
        ListenerState::NotServing { .. }
    ));
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("link metadata")
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn foreign_owned_socket_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&path).expect("socket");
    let metadata = std::fs::symlink_metadata(&path).expect("metadata");
    let uid = nix::unistd::geteuid().as_raw();
    assert_eq!(
        classify_existing(&metadata, uid.wrapping_add(1)),
        Existing::Foreign { uid }
    );
}

#[tokio::test]
async fn concurrent_starts_never_unlink_a_live_socket() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).expect("stale socket"));
    let (first, second) = tokio::join!(bind_socket(&path), bind_socket(&path));
    let (bound, error) = match (first, second) {
        (Ok(bound), Err(error)) | (Err(error), Ok(bound)) => (bound, error),
        _ => panic!("exactly one concurrent bind must win"),
    };
    assert!(matches!(error, ReplSocketError::AlreadyServing(_)));
    assert!(bound.identity.matches(&path));
    drop(bound);
    std::fs::remove_file(&path).expect("socket cleanup");
}

#[tokio::test]
async fn already_serving_reports_healthy_not_serving() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let first = test_channel(&path, 2);
    let _stream = start(&first).await;
    let second = test_channel(&path, 2);
    let _second_stream = start(&second).await;
    assert!(second.health_check().await.is_ok());
    second.shutdown().await.expect("second shutdown");
    first.shutdown().await.expect("first shutdown");
}

#[tokio::test]
async fn shutdown_does_not_unlink_foreign_socket() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    std::fs::remove_file(&path).expect("replace path");
    let replacement = std::os::unix::net::UnixListener::bind(&path).expect("replacement");
    channel.shutdown().await.expect("shutdown");
    assert!(path.exists());
    drop(replacement);
    std::fs::remove_file(&path).expect("replacement cleanup");
}

#[tokio::test]
async fn replaced_socket_inode_is_logged_and_reported_healthy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    std::fs::remove_file(&path).expect("replace path");
    let replacement = std::os::unix::net::UnixListener::bind(&path).expect("replacement");
    assert!(channel.health_check().await.is_ok());
    channel.shutdown().await.expect("shutdown");
    assert!(path.exists());
    drop(replacement);
    std::fs::remove_file(&path).expect("replacement cleanup");
}

#[tokio::test]
async fn v1_client_gets_legacy_welcome_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (_client, session) = TestClient::handshake(&path).await;
    assert_eq!(session.len(), 32);
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn vibetui4_connect_without_protocol_is_v1() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let mut client = TestClient::connect(&path).await;
    client
        .send(&ClientFrame::Connect(ConnectFrame {
            version: "vibetui4".to_string(),
            client_info: Some("opentui".to_string()),
            protocol: None,
            features: vec!["status".to_string()],
            thread: None,
        }))
        .await;
    assert!(response_text(&client.read().await).contains("Connected to test-agent"));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn handshake_times_out_without_connect() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2).with_limits(fast_limits());
    let _stream = start(&channel).await;
    let mut client = TestClient::connect(&path).await;
    assert!(client.read_optional().await.is_none());
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
#[tracing_test::traced_test]
async fn non_connect_first_frame_closes_without_logging_payload() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let mut client = TestClient::connect(&path).await;
    let secret = "handshake-payload-must-not-be-logged";
    client
        .send(&ClientFrame::Message(MessageFrame {
            content: secret.to_string(),
            session_id: None,
            msg_id: None,
            attachments: Vec::new(),
        }))
        .await;
    assert!(client.read_optional().await.is_none());
    assert!(!logs_contain(secret));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn pending_handshakes_are_capped() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let limits = ReplLimits {
        pending_handshakes: 2,
        handshake_timeout: Duration::from_millis(300),
        ..fast_limits()
    };
    let channel = test_channel(&path, 4).with_limits(limits);
    let _stream = start(&channel).await;
    let mut first = TestClient::connect(&path).await;
    let mut second = TestClient::connect(&path).await;
    let mut excess = TestClient::connect(&path).await;
    assert!(excess.read_optional().await.is_none());
    first.send(&legacy_connect()).await;
    second.send(&legacy_connect()).await;
    assert!(matches!(first.read().await, ServerFrame::Response(_)));
    assert!(matches!(second.read().await, ServerFrame::Response(_)));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn v1_server_busy_response_when_full() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 1);
    let _stream = start(&channel).await;
    let (_first, _session) = TestClient::handshake(&path).await;
    let mut second = TestClient::connect(&path).await;
    second.send(&legacy_connect()).await;
    assert!(response_text(&second.read().await).contains("Server busy: maximum connections (1)"));
    assert_eq!(channel.registry.len(), 1);
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn oversize_line_is_rejected_and_connection_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let limits = ReplLimits {
        max_frame_bytes: 64,
        ..fast_limits()
    };
    let channel = test_channel(&path, 2).with_limits(limits);
    let _stream = start(&channel).await;
    let (mut client, _session) = TestClient::handshake(&path).await;
    client
        .send(&ClientFrame::Message(MessageFrame {
            content: "x".repeat(100),
            session_id: None,
            msg_id: None,
            attachments: Vec::new(),
        }))
        .await;
    assert!(response_text(&client.read().await).contains("frame exceeds 64 bytes"));
    assert!(client.read_optional().await.is_none());
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn oversize_handshake_line_closes_connection() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let limits = ReplLimits {
        max_handshake_bytes: 32,
        ..fast_limits()
    };
    let channel = test_channel(&path, 2).with_limits(limits);
    let _stream = start(&channel).await;
    let mut client = TestClient::connect(&path).await;
    client
        .send(&ClientFrame::Connect(ConnectFrame {
            version: "x".repeat(100),
            client_info: None,
            protocol: None,
            features: Vec::new(),
            thread: None,
        }))
        .await;
    assert!(client.read_optional().await.is_none());
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn crlf_terminated_frames_are_accepted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (mut client, _session) = TestClient::handshake(&path).await;
    client.send_raw(b"\"Ping\"\r\n").await;
    assert!(matches!(client.read().await, ServerFrame::Pong));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn frame_limit_excludes_lf_and_crlf_endings() {
    for ending in ["\n", "\r\n"] {
        let line = format!("\"Ping\"{ending}");
        let mut reader = BufReader::new(line.as_bytes());
        let mut buffer = Vec::new();
        assert_eq!(
            super::conn::read_frame_line(&mut reader, &mut buffer, 6)
                .await
                .expect("frame read"),
            super::conn::FrameRead::Line
        );
        assert_eq!(buffer, b"\"Ping\"");
    }
}

#[tokio::test]
async fn v1_session_has_no_idle_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2).with_limits(fast_limits());
    let _stream = start(&channel).await;
    let (mut client, _session) = TestClient::handshake(&path).await;
    sleep(Duration::from_millis(120)).await;
    client.send(&ClientFrame::Ping).await;
    assert!(matches!(client.read().await, ServerFrame::Pong));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn respond_to_stalled_reader_returns_immediately() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let limits = ReplLimits {
        outbound_queue_frames: 2,
        outbound_queue_bytes: 1024,
        ..fast_limits()
    };
    let channel = test_channel(&path, 2).with_limits(limits);
    let _stream = start(&channel).await;
    let (_client, session) = TestClient::handshake(&path).await;
    for _ in 0..8 {
        let started = Instant::now();
        let _ = channel
            .respond(
                &incoming(&session, "hello"),
                OutgoingResponse::text("x".repeat(4096)),
            )
            .await;
        assert!(started.elapsed() < Duration::from_millis(50));
    }
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn slow_reader_is_disconnected_and_other_sessions_unaffected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let limits = ReplLimits {
        outbound_queue_frames: 2,
        outbound_queue_bytes: 64 * 1024,
        ..fast_limits()
    };
    let channel = test_channel(&path, 2).with_limits(limits);
    let _stream = start(&channel).await;
    let (_slow, slow_session) = TestClient::handshake(&path).await;
    let (mut other, _other_session) = TestClient::handshake(&path).await;
    let mut disconnected = false;
    for index in 0..100 {
        if channel
            .respond(
                &incoming(&slow_session, "hello"),
                OutgoingResponse::text("x".repeat(32 * 1024)),
            )
            .await
            .is_err()
        {
            disconnected = true;
            break;
        }
        if index % 10 == 0 {
            other.send(&ClientFrame::Ping).await;
            assert!(matches!(other.read().await, ServerFrame::Pong));
        }
    }
    assert!(disconnected, "slow consumer must be disconnected");
    other.send(&ClientFrame::Ping).await;
    assert!(matches!(other.read().await, ServerFrame::Pong));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn graceful_fatal_error_is_flushed_before_close() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let limits = ReplLimits {
        max_frame_bytes: 64,
        ..fast_limits()
    };
    let channel = test_channel(&path, 2).with_limits(limits);
    let _stream = start(&channel).await;
    let (mut client, _session) = TestClient::handshake(&path).await;
    client.send_raw(&[b'x'; 100]).await;
    assert!(response_text(&client.read().await).contains("frame exceeds"));
    assert!(client.read_optional().await.is_none());
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn v1_approval_prompt_is_a_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (mut client, session) = TestClient::handshake(&path).await;
    channel
        .send_status(
            StatusUpdate::ApprovalNeeded {
                request_id: "request-1".to_string(),
                tool_name: "shell".to_string(),
                description: "run command".to_string(),
                parameters: serde_json::json!({"command": "rm -rf build"}),
                allow_always: false,
            },
            &serde_json::json!({"unix_session_id": session}),
        )
        .await
        .expect("approval status");
    let text = response_text(&client.read().await);
    assert!(text.contains("Approval required for `shell`"), "{text}");
    assert!(text.contains("request-1"), "{text}");
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn v1_approval_prompt_marks_truncated_parameters() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (mut client, session) = TestClient::handshake(&path).await;
    channel
        .send_status(
            StatusUpdate::ApprovalNeeded {
                request_id: "request-2".to_string(),
                tool_name: "shell".to_string(),
                description: "run command".to_string(),
                parameters: serde_json::json!({"command": "x".repeat(9000)}),
                allow_always: false,
            },
            &serde_json::json!({"unix_session_id": session}),
        )
        .await
        .expect("approval status");
    let text = response_text(&client.read().await);
    assert!(
        text.contains("more bytes not shown; review the full request"),
        "{text}"
    );
    assert!(!text.contains(&"x".repeat(9000)));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn v1_keyword_without_pending_gets_notice_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (mut client, session) = TestClient::handshake(&path).await;
    channel
        .message_lifecycle(
            &incoming(&session, "ok"),
            MessageLifecycle::Finished(TurnOutcome::NoReply),
        )
        .await;
    assert_eq!(
        response_text(&client.read().await),
        "Nothing is waiting for approval."
    );
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn v1_silent_turn_gets_no_reply_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let (mut client, session) = TestClient::handshake(&path).await;
    channel
        .message_lifecycle(
            &incoming(&session, "hello"),
            MessageLifecycle::Finished(TurnOutcome::NoReply),
        )
        .await;
    assert_eq!(response_text(&client.read().await), "(no reply)");
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn v1_attachments_get_legacy_error_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let mut stream = start(&channel).await;
    let (mut client, _session) = TestClient::handshake(&path).await;
    client
        .send(&ClientFrame::Message(MessageFrame {
            content: "image".to_string(),
            session_id: None,
            msg_id: None,
            attachments: vec![WireAttachment {
                mime_type: "image/png".to_string(),
                filename: None,
                data_base64: "aGVsbG8=".to_string(),
            }],
        }))
        .await;
    assert!(response_text(&client.read().await).contains("attachments require REPLv3"));
    assert!(
        timeout(Duration::from_millis(50), stream.next())
            .await
            .is_err()
    );
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn v1_quit_is_forwarded_to_the_agent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let mut stream = start(&channel).await;
    let (mut client, _session) = TestClient::handshake(&path).await;
    client
        .send(&ClientFrame::Message(MessageFrame {
            content: "/quit".to_string(),
            session_id: None,
            msg_id: None,
            attachments: Vec::new(),
        }))
        .await;
    let message = timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("forwarded message")
        .expect("message stream");
    assert_eq!(message.content, "/quit");
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
#[tracing_test::traced_test]
async fn malformed_handshake_error_omits_payload_from_logs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let channel = test_channel(&path, 2);
    let _stream = start(&channel).await;
    let mut client = TestClient::connect(&path).await;
    let secret = "unknown-variant-payload-must-not-be-logged";
    client
        .send_raw(format!("{{\"{secret}\":{{}}}}\n").as_bytes())
        .await;
    assert!(client.read_optional().await.is_none());
    assert!(!logs_contain(secret));
    channel.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn duplicate_start_preserves_live_listener_lock() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("repl.sock");
    let first = test_channel(&path, 2);
    let _first_stream = start(&first).await;
    let _duplicate_stream = start(&first).await;
    let second = test_channel(&path, 2);
    let _second_stream = start(&second).await;
    assert!(matches!(&*second.state(), ListenerState::NotServing { .. }));
    let (_client, _session) = TestClient::handshake(&path).await;
    second.shutdown().await.expect("second shutdown");
    first.shutdown().await.expect("first shutdown");
}
