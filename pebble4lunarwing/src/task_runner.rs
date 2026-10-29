use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::executor::{self, ExecutorConfig, ExecutorEvent};
use crate::protocol::{self, Envelope, TaskRequest};
use crate::tool_proxy;

const RUNNER_LINE_SLACK_BYTES: usize = 65_536;
const CANCEL_DRAIN: Duration = Duration::from_secs(12);
const RUN_MARGIN: Duration = Duration::from_mins(1);

type RunnerResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn runner_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(message.into()))
}

fn runner_line_cap() -> RunnerResult<usize> {
    let hub_cap = match std::env::var("WS_MAX_PAYLOAD_BYTES") {
        Ok(raw) => {
            let parsed = raw.parse::<usize>()?;
            if !(1_024..=16_777_216).contains(&parsed) {
                return Err(runner_error(
                    "WS_MAX_PAYLOAD_BYTES must be between 1024 and 16777216",
                ));
            }
            parsed
        }
        Err(std::env::VarError::NotPresent) => 1_048_576,
        Err(error) => return Err(runner_error(error.to_string())),
    };
    Ok(hub_cap + RUNNER_LINE_SLACK_BYTES)
}

pub fn startup_refusal(
    uid: u32,
    token: Option<std::ffi::OsString>,
    token_file: Option<std::ffi::OsString>,
) -> Option<String> {
    let leaked =
        !token.unwrap_or_default().is_empty() || !token_file.unwrap_or_default().is_empty();
    if leaked {
        return Some("hub credential variables must never reach the task runner".to_string());
    }
    if uid == 0 {
        return Some("task runner must not run as root".to_string());
    }
    None
}

pub async fn ping(socket_path: &Path) -> RunnerResult<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = UnixStream::connect(socket_path).await?;
        stream.write_all(b"{\"op\":\"ping\"}\n").await?;
        let mut response = Vec::new();
        BufReader::new(stream.take(1025))
            .read_until(b'\n', &mut response)
            .await?;
        if response.len() > 1024 || response.last() != Some(&b'\n') {
            return Err(runner_error(
                "runner ping response must be a line of at most 1024 bytes",
            ));
        }
        let value: Value = serde_json::from_slice(&response)?;
        if value["ok"] == true && value["op"] == "pong" {
            Ok(())
        } else {
            Err(runner_error("runner did not answer ping with pong"))
        }
    })
    .await
    .map_err(|_| runner_error("runner ping timed out"))?
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn serialize_bounded(mut value: Value, payload_key: Option<&str>) -> RunnerResult<String> {
    let cap = runner_line_cap()?;
    let mut serialized = serde_json::to_string(&value)?;
    if serialized.len() <= cap {
        return Ok(serialized);
    }
    let Some(key) = payload_key else {
        return Err(runner_error(format!("runner frame exceeds {cap} bytes")));
    };
    for field in ["output", "delta", "error"] {
        let text = value[key]["payload"][field]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if text.is_empty() {
            continue;
        }
        let (mut low, mut high) = (0, text.len());
        let mut best = String::new();
        while low <= high {
            let middle = low + (high - low) / 2;
            let candidate = truncate_utf8(&text, middle);
            value[key]["payload"][field] = json!(candidate);
            let encoded = serde_json::to_string(&value)?;
            if encoded.len() <= cap {
                best = candidate;
                low = middle + 1;
            } else if middle == 0 {
                break;
            } else {
                high = middle - 1;
            }
        }
        value[key]["payload"][field] = json!(best);
        serialized = serde_json::to_string(&value)?;
        if serialized.len() <= cap {
            return Ok(serialized);
        }
    }
    Err(runner_error(format!("runner frame exceeds {cap} bytes")))
}

async fn write_line<W>(writer: &mut W, line: &str) -> RunnerResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    tokio::time::timeout(Duration::from_secs(2), async {
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await
    })
    .await
    .map_err(|_| runner_error("runner write timed out"))??;
    Ok(())
}

async fn next_line<R>(reader: &mut R, buffer: &mut Vec<u8>) -> RunnerResult<Option<String>>
where
    R: AsyncReadExt + Unpin,
{
    let cap = runner_line_cap()?;
    loop {
        if let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
            if position > cap {
                return Err(runner_error(format!("runner line exceeds {cap} bytes")));
            }
            let line: Vec<u8> = buffer.drain(..=position).collect();
            let text = String::from_utf8(line)?;
            return Ok(Some(text.trim().to_string()));
        }
        if buffer.len() > cap {
            return Err(runner_error(format!(
                "unterminated runner line exceeds {cap} bytes"
            )));
        }
        let mut chunk = [0_u8; 8192];
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            if buffer.is_empty() {
                return Ok(None);
            }
            return Err(runner_error(
                "runner connection ended with an unterminated line",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn prepare_listener(socket_path: &Path) -> RunnerResult<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    let parent = socket_path
        .parent()
        .ok_or_else(|| runner_error("task runner socket needs a parent directory"))?;
    if let Ok(metadata) = std::fs::symlink_metadata(parent) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(runner_error(
                "task runner socket directory is not a plain directory",
            ));
        }
    }
    std::fs::create_dir_all(parent)?;
    if std::env::var("LUNARWING_TASK_RUNNER_REMOTE").as_deref() == Ok("1") {
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o711))?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(socket_path) {
        if !metadata.file_type().is_socket() || metadata.uid() != tool_proxy::bridge_uid()? {
            return Err(runner_error(
                "stale runner socket is foreign or not a socket",
            ));
        }
        std::fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    if std::env::var("LUNARWING_TASK_RUNNER_REMOTE").as_deref() == Ok("1") {
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))?;
    }
    Ok(listener)
}

fn write_heartbeat(path: &Path) -> RunnerResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    let temporary = path.with_extension("tmp");
    let body = serde_json::to_vec(&json!({"ready": true, "heartbeat_at": now}))?;
    std::fs::write(&temporary, body)?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o640))?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn spawn_heartbeat(path: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = write_heartbeat(&path) {
                eprintln!("[task-runner] heartbeat write failed: {error}");
            }
            sleep(Duration::from_secs(10)).await;
        }
    })
}

pub async fn run(config: ExecutorConfig, socket_path: PathBuf) -> RunnerResult<()> {
    run_with_uid(config, socket_path, tool_proxy::bridge_uid()?).await
}

async fn run_with_uid(config: ExecutorConfig, socket_path: PathBuf, uid: u32) -> RunnerResult<()> {
    if let Some(reason) = startup_refusal(
        uid,
        std::env::var_os("AGENT_AUTH_TOKEN"),
        std::env::var_os("AGENT_AUTH_TOKEN_FILE"),
    ) {
        return Err(runner_error(reason));
    }
    if let Ok(expected) = std::env::var("PEBBLE_TASK_UID") {
        if expected.parse::<u32>().ok() != Some(uid) {
            return Err(runner_error(
                "task runner uid does not match PEBBLE_TASK_UID",
            ));
        }
    }
    let listener = prepare_listener(&socket_path)?;
    let heartbeat = std::env::var_os("LUNARWING_TASK_RUNNER_STATE_FILE")
        .map(PathBuf::from)
        .map(spawn_heartbeat);
    let config = Arc::new(config);
    let result = accept_loop(listener, config).await;
    if let Some(handle) = heartbeat {
        handle.abort();
    }
    result
}

async fn accept_loop(listener: UnixListener, config: Arc<ExecutorConfig>) -> RunnerResult<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, config).await {
                eprintln!("[task-runner] connection failed: {error}");
            }
        });
    }
}

async fn write_executor_event<W>(
    writer: &mut W,
    event: Option<ExecutorEvent>,
    task_id: &str,
) -> RunnerResult<bool>
where
    W: AsyncWriteExt + Unpin,
{
    let (op, envelope) = match event {
        Some(ExecutorEvent::Progress(envelope)) => ("progress", envelope),
        Some(ExecutorEvent::Result(envelope)) => ("result", envelope),
        None => (
            "result",
            protocol::result_envelope(
                task_id,
                "error",
                "",
                Some("executor ended without a result"),
                0,
            ),
        ),
    };
    let line = serialize_bounded(json!({"op": op, "envelope": envelope}), Some("envelope"))?;
    write_line(writer, &line).await?;
    Ok(op == "result")
}

async fn handle_connection(stream: UnixStream, config: Arc<ExecutorConfig>) -> RunnerResult<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = Vec::new();
    let Some(line) =
        tokio::time::timeout(Duration::from_secs(2), next_line(&mut reader, &mut buffer))
            .await
            .map_err(|_| runner_error("runner accept handshake timed out"))??
    else {
        return Ok(());
    };
    let request_value: Value = serde_json::from_str(&line)?;
    if request_value["op"] == "ping" {
        write_line(
            &mut write,
            &serde_json::to_string(&json!({"ok": true, "op": "pong"}))?,
        )
        .await?;
        return Ok(());
    }
    if request_value["op"] != "run" {
        write_line(
            &mut write,
            &serde_json::to_string(&json!({"ok": false, "error": "expected run"}))?,
        )
        .await?;
        return Ok(());
    }
    let request: TaskRequest = match serde_json::from_value(request_value["request"].clone()) {
        Ok(request) => request,
        Err(error) => {
            write_line(
                &mut write,
                &serde_json::to_string(&json!({"ok": false, "error": error.to_string()}))?,
            )
            .await?;
            return Ok(());
        }
    };
    let proxy_env = request_value
        .get("proxy_env")
        .filter(|value| !value.is_null())
        .cloned()
        .map(serde_json::from_value::<[(String, String); 2]>)
        .transpose()?;
    write_line(
        &mut write,
        &serde_json::to_string(&json!({"ok": true, "op": "accepted", "task_id": request.task_id}))?,
    )
    .await?;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let mut cancel_tx = Some(cancel_tx);
    let task_id = request.task_id.clone();
    let executor_task = tokio::spawn(async move {
        executor::run_task(&config, &request, event_tx, cancel_rx, proxy_env.as_ref()).await;
    });
    let mut terminal = false;
    while !terminal {
        tokio::select! {
            event = event_rx.recv() => {
                terminal = write_executor_event(&mut write, event, &task_id).await?;
            }
            line = next_line(&mut reader, &mut buffer) => {
                match line {
                    Ok(Some(text)) => {
                        let control: Value = serde_json::from_str(&text)?;
                        if control["op"] == "cancel" && control["task_id"] == task_id {
                            if let Some(sender) = cancel_tx.take() {
                                let _ = sender.send(());
                            }
                        }
                    }
                    Ok(None) => {
                        if let Some(sender) = cancel_tx.take() {
                            let _ = sender.send(());
                        }
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    drop(cancel_tx);
    if !executor_task.is_finished() && terminal {
        let _ = executor_task.await;
    }
    Ok(())
}

fn verify_socket(socket_path: &Path) -> RunnerResult<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let metadata = std::fs::symlink_metadata(socket_path)?;
    if !metadata.file_type().is_socket() {
        return Err(runner_error("task runner path is not a socket"));
    }
    if let Ok(expected) = std::env::var("LUNARWING_TASK_UID") {
        if metadata.uid() != expected.parse::<u32>()? {
            return Err(runner_error("task runner socket has an unexpected owner"));
        }
    }
    if let Ok(expected) = std::env::var("LUNARWING_TASK_GID") {
        if metadata.gid() != expected.parse::<u32>()? {
            return Err(runner_error("task runner socket has an unexpected group"));
        }
    }
    Ok(())
}

async fn connect_verified(socket_path: &Path) -> RunnerResult<UnixStream> {
    use std::os::unix::fs::MetadataExt;

    verify_socket(socket_path)?;
    let before = std::fs::symlink_metadata(socket_path)?;
    let stream = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(socket_path))
        .await
        .map_err(|_| runner_error("runner connect timed out"))??;
    verify_socket(socket_path)?;
    let after = std::fs::symlink_metadata(socket_path)?;
    if before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(runner_error("runner socket changed during connect"));
    }
    Ok(stream)
}

pub async fn delegate_task(
    socket_path: &Path,
    request: &TaskRequest,
    proxy_env: Option<&[(String, String); 2]>,
    event_tx: mpsc::UnboundedSender<ExecutorEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> RunnerResult<()> {
    let (read, mut write) = connect_verified(socket_path).await?.into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = Vec::new();
    let run =
        serde_json::to_string(&json!({"op": "run", "request": request, "proxy_env": proxy_env}))?;
    if run.len() > runner_line_cap()? {
        return Err(runner_error("runner request exceeds line cap"));
    }
    tokio::select! {
        result = write_line(&mut write, &run) => result?,
        _ = &mut cancel_rx => return Err(runner_error("task cancelled before runner dispatch")),
    }
    let accepted =
        tokio::time::timeout(Duration::from_secs(2), next_line(&mut reader, &mut buffer))
            .await
            .map_err(|_| runner_error("runner acceptance timed out"))??
            .ok_or_else(|| runner_error("runner closed before acceptance"))?;
    let accepted: Value = serde_json::from_str(&accepted)?;
    if accepted["ok"] != true
        || accepted["op"] != "accepted"
        || accepted["task_id"] != request.task_id
    {
        return Err(runner_error("runner did not accept the requested task"));
    }
    let timeout_ms = request.timeout_ms.unwrap_or(protocol::DEFAULT_TIMEOUT_MS);
    let deadline = sleep(Duration::from_millis(timeout_ms) + RUN_MARGIN);
    tokio::pin!(deadline);
    let mut cancelling = false;
    let mut timed_out = false;
    loop {
        tokio::select! {
            line = next_line(&mut reader, &mut buffer) => {
                let Some(text) = line? else {
                    return Err(runner_error("runner connection closed without a result"));
                };
                let value: Value = serde_json::from_str(&text)?;
                match value["op"].as_str().unwrap_or_default() {
                    "accepted" => {}
                    "progress" | "result" => {
                        let mut envelope: Envelope = serde_json::from_value(value["envelope"].clone())?;
                        let envelope_task = envelope.payload["task_id"].as_str().unwrap_or_default();
                        if envelope_task != request.task_id {
                            continue;
                        }
                        let terminal = value["op"] == "result";
                        let event = if terminal {
                            if timed_out {
                                envelope.payload["status"] = json!("timed_out");
                                envelope.payload["error"] = json!("task exceeded its bridge deadline");
                            }
                            ExecutorEvent::Result(envelope)
                        } else {
                            ExecutorEvent::Progress(envelope)
                        };
                        let _ = event_tx.send(event);
                        if terminal {
                            return Ok(());
                        }
                    }
                    _ => {
                        if value["ok"] == false {
                            return Err(runner_error(value["error"].as_str().unwrap_or("runner refused the task")));
                        }
                    }
                }
            }
            _ = &mut cancel_rx, if !cancelling => {
                write_line(&mut write, &serde_json::to_string(&json!({"op": "cancel", "task_id": request.task_id}))?).await?;
                cancelling = true;
                deadline.as_mut().reset((Instant::now() + CANCEL_DRAIN).into());
            }
            () = &mut deadline => {
                if cancelling {
                    return Err(runner_error("runner did not finish after cancellation"));
                }
                timed_out = true;
                write_line(&mut write, &serde_json::to_string(&json!({"op": "cancel", "task_id": request.task_id}))?).await?;
                cancelling = true;
                deadline.as_mut().reset((Instant::now() + CANCEL_DRAIN).into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ping_requires_a_pong_on_a_live_socket() {
        let root = std::env::temp_dir().join(format!("pbr-ping-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create ping root");
        let socket = root.join("runner.sock");
        let listener = UnixListener::bind(&socket).expect("bind ping listener");
        let responder = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept ping");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read ping");
            assert_eq!(line.trim(), r#"{"op":"ping"}"#);
            stream = reader.into_inner();
            stream
                .write_all(b"{\"ok\":true,\"op\":\"pong\"}\n")
                .await
                .expect("write pong");
        });
        ping(&socket)
            .await
            .expect("healthy runner responds to ping");
        responder.await.expect("responder completes");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn ping_rejects_a_socket_without_pong() {
        let root = std::env::temp_dir().join(format!("pbr-ping-bad-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create ping root");
        let socket = root.join("runner.sock");
        let listener = UnixListener::bind(&socket).expect("bind ping listener");
        let responder = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept ping");
            stream
                .write_all(b"stale\n")
                .await
                .expect("write stale reply");
        });
        assert!(ping(&socket).await.is_err());
        responder.await.expect("responder completes");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn ping_rejects_oversized_unterminated_and_silent_peers() {
        for (label, reply) in [
            ("oversized", Some(vec![b' '; 2048])),
            (
                "unterminated",
                Some(b"{\"ok\":true,\"op\":\"pong\"}".to_vec()),
            ),
            ("silent", None),
        ] {
            let root =
                std::env::temp_dir().join(format!("pbr-ping-{label}-{}", std::process::id()));
            std::fs::create_dir_all(&root).expect("create ping root");
            let socket = root.join("runner.sock");
            let listener = UnixListener::bind(&socket).expect("bind ping listener");
            let responder = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept ping");
                if let Some(reply) = reply {
                    let _ = stream.write_all(&reply).await;
                } else {
                    sleep(Duration::from_secs(10)).await;
                }
            });
            let start = Instant::now();
            assert!(ping(&socket).await.is_err(), "{label} peer must fail");
            assert!(start.elapsed() < Duration::from_secs(4));
            responder.abort();
            std::fs::remove_dir_all(root).ok();
        }
    }

    #[tokio::test]
    async fn framing_rejects_oversized_unterminated_and_non_utf8_lines() {
        let cap = runner_line_cap().expect("line cap");
        for bytes in [
            vec![b'x'; cap + 1],
            b"unterminated".to_vec(),
            vec![0xff, b'\n'],
        ] {
            let mut reader = bytes.as_slice();
            assert!(next_line(&mut reader, &mut Vec::new()).await.is_err());
        }
    }

    #[tokio::test]
    async fn delegation_bounds_acceptance_and_rejects_invalid_frames() {
        for (label, frames, succeeds) in [
            ("silent", None, false),
            ("malformed", Some("null\n".to_string()), false),
            ("closed", Some(String::new()), false),
            (
                "foreign",
                Some(format!(
                    "{}\n{}\n",
                    json!({"op":"result", "envelope":protocol::result_envelope("foreign", "success", "wrong", None, 0)}),
                    json!({"op":"result", "envelope":protocol::result_envelope("expected", "success", "correct", None, 0)}),
                )),
                true,
            ),
        ] {
            let root =
                std::env::temp_dir().join(format!("pbr-client-{label}-{}", std::process::id()));
            std::fs::create_dir_all(&root).expect("create client root");
            let socket = root.join("runner.sock");
            let listener = UnixListener::bind(&socket).expect("bind client listener");
            let responder = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("accept client");
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("read request");
                let Some(frames) = frames else {
                    sleep(Duration::from_secs(10)).await;
                    return;
                };
                let mut stream = reader.into_inner();
                stream
                    .write_all(b"{\"op\":\"accepted\",\"ok\":true,\"task_id\":\"expected\"}\n")
                    .await
                    .expect("accept run");
                stream
                    .write_all(frames.as_bytes())
                    .await
                    .expect("write frames");
            });
            let request = TaskRequest {
                task_id: "expected".to_string(),
                prompt: "probe".to_string(),
                context: crate::protocol::TaskContext::default(),
                timeout_ms: Some(10_000),
            };
            let (events, mut received) = mpsc::unbounded_channel();
            let (_cancel, cancellation) = oneshot::channel();
            let start = Instant::now();
            let result = delegate_task(&socket, &request, None, events, cancellation).await;
            assert_eq!(result.is_ok(), succeeds, "{label}: {result:?}");
            assert!(start.elapsed() < Duration::from_secs(4));
            if succeeds {
                let ExecutorEvent::Result(result) = received.recv().await.expect("result event")
                else {
                    panic!("expected result")
                };
                assert_eq!(result.payload["output"], "correct");
                assert!(received.recv().await.is_none());
            }
            responder.abort();
            std::fs::remove_dir_all(root).ok();
        }
    }

    #[test]
    fn truncation_preserves_utf8_and_bounded_serialization() {
        let value =
            json!({"op": "result", "envelope": {"payload": {"output": "a🦀".repeat(700_000)}}});
        let encoded = serialize_bounded(value, Some("envelope")).expect("bounded result");
        assert!(encoded.len() <= runner_line_cap().expect("cap"));
        assert!(encoded.is_char_boundary(encoded.len()));
    }

    #[test]
    fn startup_refuses_credentials_and_root() {
        assert!(startup_refusal(10004, Some("token".into()), None).is_some());
        assert!(startup_refusal(0, None, None).is_some());
        assert_eq!(startup_refusal(10004, None, None), None);
    }

    #[tokio::test]
    async fn real_runner_round_trip_returns_executor_result() {
        let root = std::env::temp_dir().join(format!("pbr-roundtrip-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create round-trip root");
        let stub = root.join("pebble-stub");
        crate::executor::write_executable_stub(
            &stub,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"assistant\",\"text\":\"delegated\"}'\nprintf '%s\\n' '{\"message\":\"delegated final\"}'\n",
        );
        let socket = root.join("runner.sock");
        let config = ExecutorConfig {
            pebble_bin: stub.to_string_lossy().into_owned(),
            model: "test-model".to_string(),
            permission_mode: "default".to_string(),
            workspace_root: root.to_string_lossy().into_owned(),
        };
        let server = tokio::spawn(run_with_uid(config, socket.clone(), 10004));
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        let request = TaskRequest {
            task_id: "runner-roundtrip".to_string(),
            prompt: "return the stub output".to_string(),
            context: crate::protocol::TaskContext::default(),
            timeout_ms: Some(5_000),
        };
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (_cancel_tx, cancel_rx) = oneshot::channel();
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            delegate_task(&socket, &request, None, event_tx, cancel_rx),
        )
        .await;
        server.abort();
        outcome
            .expect("round-trip deadline")
            .expect("runner result");
        let mut result = None;
        while let Some(event) = event_rx.recv().await {
            if let ExecutorEvent::Result(envelope) = event {
                result = Some(envelope);
            }
        }
        let result = result.expect("terminal executor envelope");
        assert_eq!(result.msg_type, "task_result");
        assert_eq!(result.payload["status"], "success");
        assert_eq!(result.payload["output"], "delegated final");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn delegated_cancel_returns_a_terminal_result() {
        let root = std::env::temp_dir().join(format!("pbr-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create cancel root");
        let stub = root.join("pebble-stub");
        crate::executor::write_executable_stub(
            &stub,
            "#!/bin/sh\nsleep 30 &\nprintf 'ready:%s\\n' \"$!\"\nwait\n",
        );
        let socket = root.join("runner.sock");
        let config = ExecutorConfig {
            pebble_bin: stub.to_string_lossy().into_owned(),
            model: "test-model".to_string(),
            permission_mode: "default".to_string(),
            workspace_root: root.to_string_lossy().into_owned(),
        };
        let server = tokio::spawn(run_with_uid(config, socket.clone(), 10004));
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        let request = TaskRequest {
            task_id: "runner-cancel".to_string(),
            prompt: "wait for cancellation".to_string(),
            context: crate::protocol::TaskContext::default(),
            timeout_ms: Some(30_000),
        };
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let delegated = delegate_task(&socket, &request, None, event_tx, cancel_rx);
        tokio::pin!(delegated);
        let ready = async {
            while let Some(event) = event_rx.recv().await {
                if let ExecutorEvent::Progress(envelope) = event {
                    if envelope.payload["delta"]
                        .as_str()
                        .is_some_and(|text| text.contains("ready:"))
                    {
                        return;
                    }
                }
            }
            panic!("executor ended before spawning the child");
        };
        tokio::select! {
            result = &mut delegated => panic!("delegation ended before cancellation: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(3), ready) => result.expect("spawn ready deadline"),
        }
        let _ = cancel_tx.send(());
        let outcome = tokio::time::timeout(Duration::from_secs(5), delegated).await;
        server.abort();
        outcome.expect("cancel deadline").expect("delegated task");
        let mut result = None;
        while let Some(event) = event_rx.recv().await {
            if let ExecutorEvent::Result(envelope) = event {
                result = Some(envelope);
            }
        }
        let result = result.expect("terminal cancel envelope");
        assert_eq!(result.payload["status"], "cancelled");
        assert_eq!(result.payload["task_id"], "runner-cancel");
        std::fs::remove_dir_all(root).ok();
    }
}
