use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::executor::{self, ExecutorConfig, ExecutorEvent};
use crate::protocol::{self, Envelope, TaskRequest};
use crate::tool_proxy;

const RUNNER_LINE_SLACK_BYTES: usize = 65_536;
const CANCEL_DRAIN: Duration = Duration::from_secs(12);
const RUN_MARGIN: Duration = Duration::from_secs(60);

type RunnerResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn runner_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(message.into()))
}

fn runner_line_cap() -> RunnerResult<usize> {
    let hub_cap = match std::env::var("WS_MAX_PAYLOAD_BYTES") {
        Ok(raw) => {
            let parsed = raw.parse::<usize>()?;
            if !(1_024..=16_777_216).contains(&parsed) {
                return Err(runner_error("WS_MAX_PAYLOAD_BYTES must be between 1024 and 16777216"));
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
    let leaked = !token.unwrap_or_default().is_empty() || !token_file.unwrap_or_default().is_empty();
    if leaked {
        return Some("hub credential variables must never reach the task runner".to_string());
    }
    if uid == 0 {
        return Some("task runner must not run as root".to_string());
    }
    None
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
        let text = value[key][field].as_str().unwrap_or_default().to_string();
        if text.is_empty() {
            continue;
        }
        let (mut low, mut high) = (0, text.len());
        let mut best = String::new();
        while low <= high {
            let middle = low + (high - low) / 2;
            let candidate = truncate_utf8(&text, middle);
            value[key][field] = json!(candidate);
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
        value[key][field] = json!(best);
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
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
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
            return Err(runner_error(format!("unterminated runner line exceeds {cap} bytes")));
        }
        let mut chunk = [0_u8; 8192];
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            if buffer.is_empty() {
                return Ok(None);
            }
            return Err(runner_error("runner connection ended with an unterminated line"));
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
            return Err(runner_error("task runner socket directory is not a plain directory"));
        }
    }
    std::fs::create_dir_all(parent)?;
    if std::env::var("LUNARWING_TASK_RUNNER_REMOTE").as_deref() == Ok("1") {
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o711))?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(socket_path) {
        if !metadata.file_type().is_socket() || metadata.uid() != tool_proxy::bridge_uid()? {
            return Err(runner_error("stale runner socket is foreign or not a socket"));
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
    if let Some(reason) = startup_refusal(
        tool_proxy::bridge_uid()?,
        std::env::var_os("AGENT_AUTH_TOKEN"),
        std::env::var_os("AGENT_AUTH_TOKEN_FILE"),
    ) {
        return Err(runner_error(reason));
    }
    if let Ok(expected) = std::env::var("PEBBLE_TASK_UID") {
        if expected.parse::<u32>().ok() != Some(tool_proxy::bridge_uid()?) {
            return Err(runner_error("task runner uid does not match PEBBLE_TASK_UID"));
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

async fn handle_connection(stream: UnixStream, config: Arc<ExecutorConfig>) -> RunnerResult<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = Vec::new();
    let Some(line) = next_line(&mut reader, &mut buffer).await? else {
        return Ok(());
    };
    let request_value: Value = serde_json::from_str(&line)?;
    if request_value["op"] == "ping" {
        write_line(&mut write, &serde_json::to_string(&json!({"ok": true, "op": "pong"}))?).await?;
        return Ok(());
    }
    if request_value["op"] != "run" {
        write_line(&mut write, &serde_json::to_string(&json!({"ok": false, "error": "expected run"}))?).await?;
        return Ok(());
    }
    let request: TaskRequest = match serde_json::from_value(request_value["request"].clone()) {
        Ok(request) => request,
        Err(error) => {
            write_line(&mut write, &serde_json::to_string(&json!({"ok": false, "error": error.to_string()}))?).await?;
            return Ok(());
        }
    };
    let proxy_env = if request_value.get("proxy_env").is_some() {
        match serde_json::from_value::<[(String, String); 2]>(request_value["proxy_env"].clone()) {
            Ok(pairs) => Some(pairs),
            Err(error) => {
                write_line(&mut write, &serde_json::to_string(&json!({"ok": false, "error": error.to_string()}))?).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    write_line(&mut write, &serde_json::to_string(&json!({"ok": true, "op": "accepted", "task_id": request.task_id}))?).await?;
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
                match event {
                    Some(ExecutorEvent::Progress(envelope)) => {
                        let line = serialize_bounded(json!({"op": "progress", "envelope": envelope}), Some("envelope"))?;
                        write_line(&mut write, &line).await?;
                    }
                    Some(ExecutorEvent::Result(envelope)) => {
                        let line = serialize_bounded(json!({"op": "result", "envelope": envelope}), Some("envelope"))?;
                        write_line(&mut write, &line).await?;
                        terminal = true;
                    }
                    None => {
                        let envelope = protocol::result_envelope(&task_id, "error", "", Some("executor ended without a result"), 0);
                        let line = serialize_bounded(json!({"op": "result", "envelope": envelope}), Some("envelope"))?;
                        write_line(&mut write, &line).await?;
                        terminal = true;
                    }
                }
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

pub async fn delegate_task(
    socket_path: &Path,
    request: &TaskRequest,
    proxy_env: Option<&[(String, String); 2]>,
    event_tx: mpsc::UnboundedSender<ExecutorEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> RunnerResult<()> {
    verify_socket(socket_path)?;
    let stream = UnixStream::connect(socket_path).await?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = Vec::new();
    let run = serde_json::to_string(&json!({"op": "run", "request": request, "proxy_env": proxy_env}))?;
    write_line(&mut write, &run).await?;
    let timeout_ms = request.timeout_ms.unwrap_or(protocol::DEFAULT_TIMEOUT_MS);
    let deadline = sleep(Duration::from_millis(timeout_ms) + RUN_MARGIN);
    tokio::pin!(deadline);
    let mut cancelling = false;
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
                        let envelope: Envelope = serde_json::from_value(value["envelope"].clone())?;
                        let envelope_task = envelope.payload["task_id"].as_str().unwrap_or_default();
                        if envelope_task != request.task_id {
                            continue;
                        }
                        let terminal = value["op"] == "result";
                        let event = if terminal {
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
            _ = &mut deadline => {
                if cancelling {
                    return Err(runner_error("runner did not finish after cancellation"));
                }
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

    #[test]
    fn truncation_preserves_utf8_and_bounded_serialization() {
        let value = json!({"op": "result", "envelope": {"payload": {"output": "a🦀".repeat(700_000)}}});
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
}
