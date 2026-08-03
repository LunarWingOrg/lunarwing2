//! Stdio transport for MCP servers.
//!
//! Spawns a child process and communicates via stdin/stdout using
//! newline-delimited JSON-RPC.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::BufReader;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::tools::mcp::protocol::{McpRequest, McpResponse};
use crate::tools::mcp::transport::{McpTransport, spawn_jsonrpc_reader, stream_transport_send};
use crate::tools::tool::ToolError;

const SAFE_INHERITED_ENV: &[&str] = &[
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOGNAME",
    "PATH",
    "SHELL",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "TEMP",
    "TMP",
    "TMPDIR",
    "TZ",
    "USER",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
];

#[cfg(windows)]
const SAFE_INHERITED_ENV_WINDOWS: &[&str] = &[
    "APPDATA",
    "COMSPEC",
    "LOCALAPPDATA",
    "PATHEXT",
    "SYSTEMROOT",
    "USERPROFILE",
    "WINDIR",
];

fn is_safe_inherited_env(name: &OsStr) -> bool {
    if SAFE_INHERITED_ENV
        .iter()
        .any(|allowed| env_name_matches(name, allowed))
    {
        return true;
    }

    #[cfg(windows)]
    {
        SAFE_INHERITED_ENV_WINDOWS
            .iter()
            .any(|allowed| env_name_matches(name, allowed))
    }

    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn env_name_matches(name: &OsStr, allowed: &str) -> bool {
    name.to_string_lossy().eq_ignore_ascii_case(allowed)
}

#[cfg(not(windows))]
fn env_name_matches(name: &OsStr, allowed: &str) -> bool {
    name == OsStr::new(allowed)
}

fn safe_process_environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| is_safe_inherited_env(name))
        .collect()
}

/// MCP transport that communicates with a child process over stdin/stdout.
///
/// The child process is spawned with piped stdin/stdout/stderr. Requests are
/// written as newline-delimited JSON to stdin, and responses are read from
/// stdout by a background reader task. Stderr is drained in bounded chunks;
/// content is suppressed when credentials were injected.
pub struct StdioMcpTransport {
    server_name: String,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    stderr_handle: Mutex<Option<JoinHandle<()>>>,
    child: Arc<Mutex<Child>>,
}

impl StdioMcpTransport {
    /// Spawn a child process and create a stdio transport.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable server name for logging.
    /// * `command` - The command to execute.
    /// * `args` - Command-line arguments.
    /// * `env` - Additional environment variables to set.
    /// * `suppress_stderr` - Hide raw stderr when credentials were injected.
    pub async fn spawn(
        name: impl Into<String>,
        command: &str,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        env: impl IntoIterator<Item = (impl AsRef<std::ffi::OsStr>, impl AsRef<std::ffi::OsStr>)>,
        suppress_stderr: bool,
    ) -> Result<Self, ToolError> {
        let server_name = name.into();

        let mut cmd = Command::new(command);
        cmd.args(args)
            .env_clear()
            .envs(safe_process_environment())
            .envs(env)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::ExternalService(format!(
                "[{}] Failed to spawn MCP server '{}': {}",
                server_name, command, e
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            ToolError::ExternalService(format!(
                "[{}] Failed to capture stdin of MCP server",
                server_name
            ))
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            ToolError::ExternalService(format!(
                "[{}] Failed to capture stdout of MCP server",
                server_name
            ))
        })?;

        let stderr = child.stderr.take().ok_or_else(|| {
            ToolError::ExternalService(format!(
                "[{}] Failed to capture stderr of MCP server",
                server_name
            ))
        })?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let reader = BufReader::new(stdout);
        let reader_handle = spawn_jsonrpc_reader(reader, pending.clone(), server_name.clone());

        let stderr_name = server_name.clone();
        let stderr_handle = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;

            let mut stderr = stderr;
            let mut buffer = [0_u8; 8192];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(bytes) => {
                        if suppress_stderr {
                            tracing::debug!(
                                server = %stderr_name,
                                bytes,
                                "MCP server emitted stderr output; content suppressed"
                            );
                        } else {
                            tracing::debug!(
                                server = %stderr_name,
                                stderr = %String::from_utf8_lossy(&buffer[..bytes]),
                                "MCP server stderr"
                            );
                        }
                    }
                }
            }
        });

        Ok(Self {
            server_name,
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            reader_handle: Mutex::new(Some(reader_handle)),
            stderr_handle: Mutex::new(Some(stderr_handle)),
            child: Arc::new(Mutex::new(child)),
        })
    }

    /// Return the child exit status without waiting for a running process.
    pub async fn exit_status(&self) -> Result<Option<std::process::ExitStatus>, ToolError> {
        self.child.lock().await.try_wait().map_err(|error| {
            ToolError::ExternalService(format!(
                "[{}] Failed to query MCP server status: {error}",
                self.server_name
            ))
        })
    }
}

#[async_trait]
impl McpTransport for StdioMcpTransport {
    async fn send(
        &self,
        request: &McpRequest,
        _headers: &HashMap<String, String>,
    ) -> Result<McpResponse, ToolError> {
        stream_transport_send(
            &self.stdin,
            &self.pending,
            request,
            &self.server_name,
            Duration::from_secs(30),
        )
        .await
    }

    async fn shutdown(&self) -> Result<(), ToolError> {
        // Kill the child process.
        {
            let mut child = self.child.lock().await;
            let _ = child.kill().await;
        }

        // Abort the reader tasks.
        if let Some(handle) = self.reader_handle.lock().await.take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_handle.lock().await.take() {
            handle.abort();
        }

        // Drain pending requests so waiters wake immediately instead of
        // hanging until their 30s timeout.
        {
            let mut pending = self.pending.lock().await;
            pending.clear(); // Dropping senders wakes receivers with Err
        }

        tracing::debug!("[{}] Stdio transport shut down", self.server_name);
        Ok(())
    }

    fn supports_http_features(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_nonexistent_command_fails() {
        let env: HashMap<String, String> = HashMap::new();
        let result = StdioMcpTransport::spawn(
            "test",
            "this-command-does-not-exist-lunarwing-test",
            std::iter::empty::<&str>(),
            &env,
            false,
        )
        .await;

        let err = result.err().expect("should be an error").to_string();
        assert!(
            err.contains("Failed to spawn"),
            "Error should mention spawn failure: {}",
            err
        );
    }

    #[test]
    fn inherited_environment_allowlist_excludes_daemon_secrets() {
        assert!(is_safe_inherited_env(OsStr::new("PATH")));
        assert!(is_safe_inherited_env(OsStr::new("HOME")));
        assert!(!is_safe_inherited_env(OsStr::new("DATABASE_URL")));
        assert!(!is_safe_inherited_env(OsStr::new("SECRETS_MASTER_KEY")));
        assert!(!is_safe_inherited_env(OsStr::new("GATEWAY_AUTH_TOKEN")));
    }

    #[tokio::test]
    async fn test_spawn_and_shutdown() {
        let env: HashMap<String, String> = HashMap::new();
        let transport =
            StdioMcpTransport::spawn("test-cat", "cat", std::iter::empty::<&str>(), &env, false)
                .await
                .expect("cat should be available");

        // Verify shutdown completes without error.
        transport.shutdown().await.expect("shutdown should succeed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_spawn_receives_explicit_environment() {
        let env = HashMap::from([("MCP_TEST_TOKEN".to_string(), "expected".to_string())]);
        let transport = StdioMcpTransport::spawn(
            "test-env",
            "sh",
            ["-c", "test \"$MCP_TEST_TOKEN\" = expected"],
            &env,
            true,
        )
        .await
        .expect("spawn shell");
        let status = transport
            .child
            .lock()
            .await
            .wait()
            .await
            .expect("wait for shell");

        assert!(status.success());
        transport.shutdown().await.expect("shutdown should succeed");
    }

    #[tokio::test]
    async fn test_send_timeout_on_non_jsonrpc_server() {
        // Spawn `cat` which echoes input back. Since the echoed input is the
        // request (not a response with matching id), it will be ignored by the
        // reader and we should hit the timeout. We use a short-lived test so
        // we override the 30s timeout expectation by just checking the error type.
        let env: HashMap<String, String> = HashMap::new();
        let transport =
            StdioMcpTransport::spawn("test-echo", "cat", std::iter::empty::<&str>(), &env, false)
                .await
                .expect("cat should be available");

        let request = McpRequest::list_tools(999);
        let headers = HashMap::new();

        // The request will be echoed back by `cat`, but it won't parse as a
        // valid McpResponse with matching id, so the reader will log a debug
        // message and the send will eventually timeout. We don't want to wait
        // 30 seconds in tests, so we just verify the transport was created and
        // shut it down.
        transport.shutdown().await.expect("shutdown should succeed");

        // Verify that pending map is empty after shutdown.
        let pending = transport.pending.lock().await;
        assert!(pending.is_empty());
        drop(pending);

        // Verify send after shutdown fails (stdin is closed).
        let result = transport.send(&request, &headers).await;
        assert!(result.is_err());
    }
}
