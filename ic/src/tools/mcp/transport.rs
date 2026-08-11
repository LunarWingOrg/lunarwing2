//! Shared MCP transport trait and JSON-RPC framing helpers.
//!
//! Provides the [`McpTransport`] trait that all MCP transports implement,
//! plus `write_jsonrpc_line` and `spawn_jsonrpc_reader` for newline-delimited
//! JSON-RPC over byte streams (used by stdio and unix socket transports).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::tools::mcp::protocol::{
    InboundJsonRpc, McpRequest, McpResponse, classify_inbound_jsonrpc,
};
use crate::tools::tool::ToolError;

const MAX_JSONRPC_LINE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct McpNotificationState {
    tools_list_changed_enabled: AtomicBool,
    tools_generation: AtomicU64,
}

impl McpNotificationState {
    pub(crate) fn set_tools_list_changed(&self, enabled: bool) {
        self.tools_list_changed_enabled
            .store(enabled, Ordering::Release);
    }

    pub(crate) fn tools_generation(&self) -> u64 {
        self.tools_generation.load(Ordering::Acquire)
    }

    pub(crate) fn tools_list_changed_enabled(&self) -> bool {
        self.tools_list_changed_enabled.load(Ordering::Acquire)
    }

    pub(crate) fn advance_tools_generation(&self) {
        if self.tools_list_changed_enabled.load(Ordering::Acquire) {
            let _ = self.tools_generation.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |generation| Some(generation.saturating_add(1)),
            );
        }
    }

    pub(crate) fn handle_notification(&self, method: &str) -> bool {
        if method != "notifications/tools/list_changed"
            || !self.tools_list_changed_enabled.load(Ordering::Acquire)
        {
            return false;
        }
        self.advance_tools_generation();
        true
    }
}

/// Trait for sending JSON-RPC requests to an MCP server and receiving responses.
///
/// Implementations handle the underlying transport (HTTP, stdio, unix socket, etc.).
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a request and wait for the corresponding response.
    ///
    /// `headers` are used by HTTP-based transports (e.g., `Mcp-Session-Id`);
    /// stream-based transports may ignore them.
    async fn send(
        &self,
        request: &McpRequest,
        headers: &HashMap<String, String>,
    ) -> Result<McpResponse, ToolError>;

    /// Shut down the transport, releasing any resources (child processes, connections).
    async fn shutdown(&self) -> Result<(), ToolError>;

    /// Shut down using request headers when the transport has a remote session to terminate.
    async fn shutdown_with_headers(
        &self,
        _headers: &HashMap<String, String>,
    ) -> Result<(), ToolError> {
        self.shutdown().await
    }

    /// Whether this transport supports HTTP-specific features like session headers.
    fn supports_http_features(&self) -> bool {
        false
    }

    /// Whether a persistent in-process runtime can still accept requests.
    /// Other transports keep their existing lifecycle-specific health checks.
    fn is_healthy(&self) -> bool {
        true
    }

    /// Configure and inspect bounded tools/list change signaling.
    fn set_tools_list_changed(&self, _enabled: bool) {}

    fn tools_generation(&self) -> u64 {
        0
    }

    /// Start transport-specific delivery of server-originated notifications.
    async fn start_notifications(
        &self,
        _headers: &HashMap<String, String>,
    ) -> Result<(), ToolError> {
        Ok(())
    }
}

/// Serialize an [`McpRequest`] as a single JSON line and write it to `writer`.
///
/// The line is terminated with `\n` and the writer is flushed.
pub async fn write_jsonrpc_line(
    writer: &mut (impl AsyncWrite + Unpin),
    request: &McpRequest,
) -> Result<(), ToolError> {
    let json = serde_json::to_string(request).map_err(|e| {
        ToolError::ExternalService(format!("Failed to serialize JSON-RPC request: {e}"))
    })?;

    writer.write_all(json.as_bytes()).await.map_err(|e| {
        ToolError::ExternalService(format!("Failed to write JSON-RPC request: {e}"))
    })?;

    writer
        .write_all(b"\n")
        .await
        .map_err(|e| ToolError::ExternalService(format!("Failed to write newline: {e}")))?;

    writer
        .flush()
        .await
        .map_err(|e| ToolError::ExternalService(format!("Failed to flush JSON-RPC writer: {e}")))?;

    Ok(())
}

/// Spawn a background task that reads newline-delimited JSON-RPC responses from
/// `reader` and dispatches them to the matching pending sender in `pending`.
///
/// Each line is parsed as an [`McpResponse`]. If the response has an `id` that
/// matches a pending request, the corresponding [`oneshot::Sender`] is resolved.
/// Parse failures are logged at debug level and skipped.
pub fn spawn_jsonrpc_reader<R: AsyncBufRead + Unpin + Send + 'static>(
    reader: R,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>>,
    notifications: Arc<McpNotificationState>,
    server_name: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = reader;
        loop {
            let line = match read_bounded_jsonrpc_line(&mut reader).await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(
                        "[{}] Stopping JSON-RPC reader after invalid frame: {}",
                        server_name,
                        error
                    );
                    break;
                }
            };
            let response = match classify_inbound_jsonrpc(&line) {
                Ok(InboundJsonRpc::Response(response)) => response,
                Ok(InboundJsonRpc::Notification { method }) => {
                    if !notifications.handle_notification(&method) {
                        tracing::debug!(server = %server_name, %method, "Ignoring unsupported MCP notification");
                    }
                    continue;
                }
                Ok(InboundJsonRpc::Request { id, method }) => {
                    tracing::warn!(server = %server_name, id, %method, "Ignoring unsupported server-originated MCP request");
                    continue;
                }
                Err(e) => {
                    // Truncate logged line to avoid leaking sensitive data in large payloads.
                    let preview = String::from_utf8_lossy(&line[..line.len().min(200)]);
                    tracing::debug!(
                        "[{}] Failed to parse JSON-RPC response: {} — line: {}{}",
                        server_name,
                        e,
                        preview,
                        if line.len() > 200 { "…" } else { "" }
                    );
                    continue;
                }
            };

            let Some(id) = response.id else { continue };
            let mut map = pending.lock().await;
            if let Some(tx) = map.remove(&id) {
                // Ignore send error — the receiver may have been dropped (timeout).
                let _ = tx.send(response);
            } else {
                tracing::debug!(
                    "[{}] Received response for unknown request id {}",
                    server_name,
                    id
                );
            }
        }

        pending.lock().await.clear();
        tracing::debug!("[{}] JSON-RPC reader finished", server_name);
    })
}

async fn read_bounded_jsonrpc_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk_len = newline.unwrap_or(available.len());
        if line.len().saturating_add(chunk_len) > MAX_JSONRPC_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("JSON-RPC line exceeds {MAX_JSONRPC_LINE_BYTES} bytes"),
            ));
        }
        line.extend_from_slice(&available[..chunk_len]);
        let consumed = chunk_len + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

/// Send a JSON-RPC request over a stream-based transport (stdio / unix socket).
///
/// Handles notification fire-and-forget, pending response registration,
/// write, timeout, and cleanup. Used by both [`StdioMcpTransport`] and
/// [`UnixMcpTransport`] to avoid duplicating the send logic.
pub(crate) async fn stream_transport_send<W: AsyncWrite + Unpin>(
    writer: &Mutex<W>,
    pending: &Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>,
    request: &McpRequest,
    server_name: &str,
    timeout_duration: std::time::Duration,
) -> Result<McpResponse, ToolError> {
    // JSON-RPC notifications (no id) are fire-and-forget: the server
    // will not send a response, so we must not wait for one.
    if request.id.is_none() {
        let mut w = writer.lock().await;
        write_jsonrpc_line(&mut *w, request).await?;
        return Ok(McpResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: None,
        });
    }

    let id = request.id.unwrap_or(0);
    let (tx, rx) = oneshot::channel();

    // Register the pending response handler before writing the request,
    // so we don't miss a fast response from the server.
    {
        let mut map = pending.lock().await;
        map.insert(id, tx);
    }

    // Write the request.
    {
        let mut w = writer.lock().await;
        if let Err(e) = write_jsonrpc_line(&mut *w, request).await {
            // Remove the pending entry on write failure.
            let mut map = pending.lock().await;
            map.remove(&id);
            return Err(e);
        }
    }

    // Wait for the response with a timeout.
    match tokio::time::timeout(timeout_duration, rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => {
            // Sender was dropped (reader task ended). Clean up pending entry.
            let mut map = pending.lock().await;
            map.remove(&id);
            Err(ToolError::ExternalService(format!(
                "[{}] MCP server closed connection before responding to request {:?}",
                server_name, request.id
            )))
        }
        Err(_) => {
            // Timeout: remove the pending entry.
            let mut map = pending.lock().await;
            map.remove(&id);
            Err(ToolError::ExternalService(format!(
                "[{}] Timeout waiting for response to request {:?} after {:?}",
                server_name, request.id, timeout_duration
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_jsonrpc_line_serializes_and_flushes() {
        let request = McpRequest {
            jsonrpc: "2.0".into(),
            id: Some(1),
            method: "test/method".into(),
            params: None,
        };

        let mut buf = Vec::new();
        write_jsonrpc_line(&mut buf, &request)
            .await
            .expect("write should succeed");

        let written = String::from_utf8(buf).expect("should be valid UTF-8");
        assert!(written.ends_with('\n'));

        let parsed: serde_json::Value =
            serde_json::from_str(written.trim()).expect("should be valid JSON");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["method"], "test/method");
    }

    #[tokio::test]
    async fn test_spawn_jsonrpc_reader_dispatches_response() {
        let response = McpResponse {
            jsonrpc: "2.0".into(),
            id: Some(42),
            result: Some(serde_json::json!({"tools": []})),
            error: None,
        };
        let line = format!("{}\n", serde_json::to_string(&response).unwrap());

        let reader = std::io::Cursor::new(line.into_bytes());
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = oneshot::channel();
        {
            let mut map = pending.lock().await;
            map.insert(42, tx);
        }

        let handle =
            spawn_jsonrpc_reader(reader, pending.clone(), notification_state(), "test".into());

        let resp = rx.await.expect("should receive response");
        assert_eq!(resp.id, Some(42));
        assert!(resp.result.is_some());

        handle.await.expect("reader task should finish");
    }

    #[tokio::test]
    async fn test_spawn_jsonrpc_reader_skips_invalid_lines() {
        let input = "this is not json\n{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":null}\n";
        let reader = std::io::Cursor::new(input.as_bytes().to_vec());
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = oneshot::channel();
        {
            let mut map = pending.lock().await;
            map.insert(7, tx);
        }

        let handle =
            spawn_jsonrpc_reader(reader, pending.clone(), notification_state(), "test".into());

        let resp = rx
            .await
            .expect("should receive response despite earlier invalid line");
        assert_eq!(resp.id, Some(7));

        handle.await.expect("reader task should finish");
    }

    #[tokio::test]
    async fn jsonrpc_reader_rejects_oversized_line_and_drops_pending_requests() {
        let reader = std::io::Cursor::new(vec![b'x'; MAX_JSONRPC_LINE_BYTES + 1]);
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(1, tx);

        let handle = spawn_jsonrpc_reader(
            reader,
            Arc::clone(&pending),
            notification_state(),
            "test".into(),
        );
        handle.await.expect("reader task should finish");

        assert!(rx.await.is_err());
        assert!(pending.lock().await.is_empty());
    }

    /// Issue 9 regression: a JSON-RPC notification (no id) must not resolve
    /// a pending request keyed by id 0 (the old `unwrap_or(0)` default).
    #[tokio::test]
    async fn test_notification_does_not_resolve_pending_id_zero() {
        // A notification response (no id), followed by a proper response for id 0.
        let notification = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#;
        let real_response = r#"{"jsonrpc":"2.0","id":0,"result":{"ok":true}}"#;
        let input = format!("{notification}\n{real_response}\n");

        let reader = std::io::Cursor::new(input.into_bytes());
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = oneshot::channel();
        {
            let mut map = pending.lock().await;
            map.insert(0, tx);
        }

        let handle =
            spawn_jsonrpc_reader(reader, pending.clone(), notification_state(), "test".into());

        let resp = rx.await.expect("should receive the real id=0 response");
        assert_eq!(resp.id, Some(0));
        assert!(resp.result.is_some());

        handle.await.expect("reader task should finish");
    }

    #[tokio::test]
    async fn colliding_server_request_does_not_resolve_pending_response() {
        let request = r#"{"jsonrpc":"2.0","id":7,"method":"sampling/createMessage","params":{}}"#;
        let response = r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
        let reader = std::io::Cursor::new(format!("{request}\n{response}\n").into_bytes());
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(7, tx);

        let handle = spawn_jsonrpc_reader(
            reader,
            Arc::clone(&pending),
            notification_state(),
            "test".into(),
        );
        let received = rx
            .await
            .expect("actual response must satisfy pending request");
        assert_eq!(received.result, Some(serde_json::json!({"ok": true})));
        handle.await.expect("reader finishes");
    }

    #[tokio::test]
    async fn list_changed_signal_requires_exact_method_and_capability() {
        let state = notification_state();
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed_extra\"}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n",
        );
        spawn_jsonrpc_reader(
            std::io::Cursor::new(input.as_bytes().to_vec()),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::clone(&state),
            "test".into(),
        )
        .await
        .unwrap();
        assert_eq!(state.tools_generation(), 0);

        state.set_tools_list_changed(true);
        spawn_jsonrpc_reader(
            std::io::Cursor::new(input.as_bytes().to_vec()),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::clone(&state),
            "test".into(),
        )
        .await
        .unwrap();
        assert_eq!(state.tools_generation(), 1);
    }

    fn notification_state() -> Arc<McpNotificationState> {
        Arc::new(McpNotificationState::default())
    }
}
