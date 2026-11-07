use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use lunarwing_common::repl_wire::{
    ClientFrame, ConnectFrame, ResponseFrame, ServerFrame, encode_client_line,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

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
