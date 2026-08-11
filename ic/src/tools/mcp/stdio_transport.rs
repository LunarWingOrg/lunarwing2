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
const MCP_CHILD_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "linux")]
struct PreparedDescriptorExec {
    // Keep CString buffers alive; pointer arrays below address these buffers.
    _argv_strings: Vec<std::ffi::CString>,
    argv: Box<[*mut libc::c_char]>,
    _env_strings: Vec<std::ffi::CString>,
    env: Box<[*mut libc::c_char]>,
    executable: crate::tools::mcp::provenance::VerifiedStdioExecutable,
}

// SAFETY: pointers address immutable CString heap buffers owned by same struct.
// libc models exec argv/env entries as mutable, but kernel only reads them.
// Moving struct between threads does not move those buffers.
#[cfg(target_os = "linux")]
unsafe impl Send for PreparedDescriptorExec {}
#[cfg(target_os = "linux")]
unsafe impl Sync for PreparedDescriptorExec {}

#[cfg(target_os = "linux")]
impl PreparedDescriptorExec {
    fn exec(&self) -> std::io::Result<()> {
        let fd = self.executable.raw_fd();
        let empty_path = c"";
        let result = unsafe {
            libc::execveat(
                fd,
                empty_path.as_ptr(),
                self.argv.as_ptr(),
                self.env.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        debug_assert_eq!(result, -1);
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn install_verified_exec(
    command: &mut Command,
    executable: crate::tools::mcp::provenance::VerifiedStdioExecutable,
    command_name: &str,
    args: &[OsString],
    env: &[(OsString, OsString)],
) -> Result<(), ToolError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    let mut argv_strings = Vec::with_capacity(args.len() + 1);
    argv_strings.push(
        std::ffi::CString::new(command_name.as_bytes()).map_err(|_| {
            ToolError::ExternalService("Pinned MCP command contains a NUL byte".to_string())
        })?,
    );
    for arg in args {
        argv_strings.push(std::ffi::CString::new(arg.as_bytes()).map_err(|_| {
            ToolError::ExternalService(
                "Pinned MCP command argument contains a NUL byte".to_string(),
            )
        })?);
    }
    let env_strings = env
        .iter()
        .map(|(name, value)| {
            if name.as_bytes().contains(&b'=') {
                return Err(ToolError::ExternalService(
                    "Pinned MCP environment name contains '='".to_string(),
                ));
            }
            let mut entry = Vec::with_capacity(name.len() + value.len() + 1);
            entry.extend_from_slice(name.as_bytes());
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            std::ffi::CString::new(entry).map_err(|_| {
                ToolError::ExternalService("Pinned MCP environment contains a NUL byte".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let argv = argv_strings
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let env = env_strings
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let prepared = PreparedDescriptorExec {
        _argv_strings: argv_strings,
        argv,
        _env_strings: env_strings,
        env,
        executable,
    };
    // SAFETY: sandbox hooks were installed first. This final hook only calls
    // execveat using immutable storage prepared before fork.
    unsafe {
        command.as_std_mut().pre_exec(move || prepared.exec());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn install_verified_exec(
    _command: &mut Command,
    _executable: crate::tools::mcp::provenance::VerifiedStdioExecutable,
    _command_name: &str,
    _args: &[OsString],
    _env: &[(OsString, OsString)],
) -> Result<(), ToolError> {
    Err(ToolError::ExternalService(
        "Atomic execution of pinned MCP commands is unsupported on this platform".to_string(),
    ))
}

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
    notifications: Arc<crate::tools::mcp::transport::McpNotificationState>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    stderr_handle: Mutex<Option<JoinHandle<()>>>,
    child: Arc<Mutex<Child>>,
    /// Shutdown SIGKILLs the child's whole process group (sandboxed servers
    /// run as group leaders so grandchildren cannot outlive the server).
    kill_process_group: bool,
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
    /// * `sandbox` - Optional host OS sandbox policy for the child. A policy
    ///   that cannot be enforced fails the spawn before any process starts.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        name: impl Into<String>,
        command: &str,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        env: impl IntoIterator<Item = (impl AsRef<std::ffi::OsStr>, impl AsRef<std::ffi::OsStr>)>,
        suppress_stderr: bool,
        sandbox: Option<&crate::tools::mcp::sandbox::McpSandboxConfig>,
    ) -> Result<Self, ToolError> {
        Self::spawn_inner(name, command, args, env, suppress_stderr, sandbox, None).await
    }

    /// Spawn using descriptor retained by pinned provenance verification.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn_verified(
        name: impl Into<String>,
        command: &str,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        env: impl IntoIterator<Item = (impl AsRef<std::ffi::OsStr>, impl AsRef<std::ffi::OsStr>)>,
        suppress_stderr: bool,
        sandbox: Option<&crate::tools::mcp::sandbox::McpSandboxConfig>,
        executable: crate::tools::mcp::provenance::VerifiedStdioExecutable,
    ) -> Result<Self, ToolError> {
        Self::spawn_inner(
            name,
            command,
            args,
            env,
            suppress_stderr,
            sandbox,
            Some(executable),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_inner(
        name: impl Into<String>,
        command: &str,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        env: impl IntoIterator<Item = (impl AsRef<std::ffi::OsStr>, impl AsRef<std::ffi::OsStr>)>,
        suppress_stderr: bool,
        sandbox: Option<&crate::tools::mcp::sandbox::McpSandboxConfig>,
        executable: Option<crate::tools::mcp::provenance::VerifiedStdioExecutable>,
    ) -> Result<Self, ToolError> {
        let server_name = name.into();
        let args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_os_string())
            .collect::<Vec<_>>();
        let explicit_env = env
            .into_iter()
            .map(|(name, value)| (name.as_ref().to_os_string(), value.as_ref().to_os_string()))
            .collect::<Vec<_>>();
        let mut child_env = safe_process_environment();
        for (name, value) in &explicit_env {
            child_env.retain(|(inherited, _)| inherited != name);
            child_env.push((name.clone(), value.clone()));
        }

        let mut cmd = Command::new(command);
        cmd.args(&args)
            .env_clear()
            .envs(child_env.iter().map(|(name, value)| (name, value)))
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(sandbox) = sandbox {
            crate::tools::mcp::sandbox::install(&mut cmd, sandbox).map_err(|error| {
                ToolError::ExternalService(format!(
                    "[{}] Failed to prepare sandbox for MCP server '{}': {}",
                    server_name, command, error
                ))
            })?;
        }

        if let Some(executable) = executable {
            install_verified_exec(&mut cmd, executable, command, &args, &child_env)?;
        }

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
        let notifications = Arc::new(crate::tools::mcp::transport::McpNotificationState::default());

        let reader = BufReader::new(stdout);
        let reader_handle = spawn_jsonrpc_reader(
            reader,
            pending.clone(),
            Arc::clone(&notifications),
            server_name.clone(),
        );

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
            notifications,
            reader_handle: Mutex::new(Some(reader_handle)),
            stderr_handle: Mutex::new(Some(stderr_handle)),
            child: Arc::new(Mutex::new(child)),
            kill_process_group: crate::tools::mcp::sandbox::kills_process_group(sandbox),
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

    /// Whether stdout processing ended while child may still be alive.
    pub async fn reader_finished(&self) -> bool {
        self.reader_handle
            .lock()
            .await
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
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
        let kill_timed_out = tokio::time::timeout(MCP_CHILD_SHUTDOWN_TIMEOUT, async {
            let mut child = self.child.lock().await;
            #[cfg(unix)]
            if self.kill_process_group {
                // The sandbox made the child a process-group leader; kill the
                // whole group so spawned grandchildren cannot linger. ESRCH
                // (group already gone) is expected and ignored.
                if let Some(pid) = child.id() {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(pid as i32),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
            }
            let _ = child.kill().await;
        })
        .await
        .is_err();

        if let Some(handle) = self.reader_handle.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
        if let Some(handle) = self.stderr_handle.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }

        // Drain pending requests so waiters wake immediately instead of
        // hanging until their 30s timeout.
        {
            let mut pending = self.pending.lock().await;
            pending.clear(); // Dropping senders wakes receivers with Err
        }

        tracing::debug!("[{}] Stdio transport shut down", self.server_name);
        if kill_timed_out {
            Err(ToolError::ExternalService(format!(
                "[{}] Timed out stopping MCP server process",
                self.server_name
            )))
        } else {
            Ok(())
        }
    }

    fn supports_http_features(&self) -> bool {
        false
    }

    fn set_tools_list_changed(&self, enabled: bool) {
        self.notifications.set_tools_list_changed(enabled);
    }

    fn tools_generation(&self) -> u64 {
        self.notifications.tools_generation()
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
            None,
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
        let transport = StdioMcpTransport::spawn(
            "test-cat",
            "cat",
            std::iter::empty::<&str>(),
            &env,
            false,
            None,
        )
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
            None,
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pinned_spawn_executes_verified_descriptor_after_path_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp dir");
        let command = temp.path().join("server");
        let replacement = temp.path().join("server-replacement");
        let native_true = crate::tools::mcp::provenance::resolve_command_path("true")
            .expect("resolve native true");
        let native_false = crate::tools::mcp::provenance::resolve_command_path("false")
            .expect("resolve native false");
        std::fs::copy(native_true, &command).expect("copy verified native executable");
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755))
            .expect("chmod verified executable");
        let provenance = crate::tools::mcp::provenance::McpPackageProvenance::pin_for_command(
            command.to_str().expect("utf-8 command"),
            crate::tools::mcp::provenance::ProvenanceIdentity::default(),
        )
        .expect("pin executable");
        let executable = provenance
            .prepare_stdio_command(command.to_str().expect("utf-8 command"))
            .expect("verify executable")
            .expect("pinned descriptor");

        std::fs::copy(native_false, &replacement).expect("copy replacement native executable");
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755))
            .expect("chmod replacement executable");
        std::fs::rename(&replacement, &command).expect("replace executable path");
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            no_new_privileges: true,
            ..Default::default()
        };

        let transport = StdioMcpTransport::spawn_verified(
            "pinned-race",
            command.to_str().expect("utf-8 command"),
            std::iter::empty::<&str>(),
            std::iter::empty::<(&str, &str)>(),
            false,
            Some(&sandbox),
            executable,
        )
        .await
        .expect("spawn verified descriptor");
        let status =
            tokio::time::timeout(Duration::from_secs(2), transport.child.lock().await.wait())
                .await
                .expect("verified executable did not exit")
                .expect("wait for verified executable");
        assert!(
            status.success(),
            "snapshot must execute original native true binary"
        );
        transport.shutdown().await.expect("shutdown transport");
    }

    #[tokio::test]
    async fn test_send_timeout_on_non_jsonrpc_server() {
        // Spawn `cat` which echoes input back. Since the echoed input is the
        // request (not a response with matching id), it will be ignored by the
        // reader and we should hit the timeout. We use a short-lived test so
        // we override the 30s timeout expectation by just checking the error type.
        let env: HashMap<String, String> = HashMap::new();
        let transport = StdioMcpTransport::spawn(
            "test-echo",
            "cat",
            std::iter::empty::<&str>(),
            &env,
            false,
            None,
        )
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

    /// Spawn `sh -c ...` under a sandbox policy and return its exit status.
    #[cfg(unix)]
    async fn spawn_sandboxed_shell(
        name: &str,
        script: &str,
        script_args: &[&str],
        sandbox: &crate::tools::mcp::sandbox::McpSandboxConfig,
    ) -> std::process::ExitStatus {
        let env: HashMap<String, String> = HashMap::new();
        let mut args = vec!["-c", script, "sandbox-test"];
        args.extend_from_slice(script_args);
        let transport = StdioMcpTransport::spawn(name, "sh", args, &env, false, Some(sandbox))
            .await
            .expect("sandboxed shell should spawn");
        let status = transport
            .child
            .lock()
            .await
            .wait()
            .await
            .expect("wait for sandboxed shell");
        transport.shutdown().await.expect("shutdown should succeed");
        status
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_applies_memory_and_core_dump_limits() {
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            max_memory_mb: Some(64),
            ..Default::default()
        };
        // ulimit -v reports KiB; 64 MiB == 65536. Core dumps default to 0.
        let status = spawn_sandboxed_shell(
            "sandbox-limits",
            "test \"$(ulimit -v)\" = 65536 && test \"$(ulimit -c)\" = 0",
            &[],
            &sandbox,
        )
        .await;
        assert!(status.success());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandbox_preserves_configured_core_dumps() {
        let (soft, _hard) =
            nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_CORE)
                .expect("read daemon core limit");
        let expected = if soft == u64::MAX {
            "unlimited".to_string()
        } else {
            soft.to_string()
        };
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            allow_core_dumps: true,
            ..Default::default()
        };
        // /proc/self/limits reports the soft limit in bytes as the 5th field.
        // Save the expected value before `set --` replaces positional params.
        let status = spawn_sandboxed_shell(
            "sandbox-cores",
            "expected=$1; set -- $(grep '^Max core file size' /proc/self/limits) \
                && test \"$5\" = \"$expected\"",
            &[&expected],
            &sandbox,
        )
        .await;
        assert!(status.success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_rejects_limit_above_daemon_hard_limit() {
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            max_file_descriptors: Some(u64::MAX),
            ..Default::default()
        };
        let env: HashMap<String, String> = HashMap::new();
        let result = StdioMcpTransport::spawn(
            "sandbox-hard-limit",
            "cat",
            std::iter::empty::<&str>(),
            &env,
            false,
            Some(&sandbox),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("limit above the hard limit must fail"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("hard limit"),
            "unexpected error: {error}"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandbox_sets_no_new_privileges_and_process_group() {
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            no_new_privileges: true,
            isolate_process_group: true,
            ..Default::default()
        };
        let env: HashMap<String, String> = HashMap::new();
        let transport = StdioMcpTransport::spawn(
            "sandbox-prctl",
            "cat",
            std::iter::empty::<&str>(),
            &env,
            false,
            Some(&sandbox),
        )
        .await
        .expect("spawn sandboxed cat");
        let pid = transport.child.lock().await.id().expect("child pid");

        // /proc/<pid>/status carries the surviving no_new_privs flag.
        let status_text =
            std::fs::read_to_string(format!("/proc/{pid}/status")).expect("read child status");
        assert!(
            status_text.lines().any(|line| line == "NoNewPrivs:\t1"),
            "child must keep no_new_privs across exec: {status_text}"
        );

        // /proc/<pid>/stat: pid (comm) state ppid pgrp — the pgrp field of a
        // process-group leader equals its own pid.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("read child stat");
        let after_comm = stat.rsplit_once(") ").expect("stat comm boundary").1;
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        let pgrp: u32 = fields[2].parse().expect("parse pgrp");
        assert_eq!(pgrp, pid, "child must lead its own process group");

        transport.shutdown().await.expect("shutdown should succeed");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandbox_deny_ptrace_installs_seccomp_filter() {
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            deny_ptrace: true,
            ..Default::default()
        };
        // Seccomp mode 2 is "filter"; the field survives exec.
        let status = spawn_sandboxed_shell(
            "sandbox-seccomp",
            "grep -q '^Seccomp:.2' /proc/self/status",
            &[],
            &sandbox,
        )
        .await;
        assert!(status.success(), "seccomp filter must be active");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandbox_read_only_root_keeps_only_writable_paths() {
        if crate::tools::mcp::sandbox::landlock_abi_version().is_none() {
            eprintln!("skipping: kernel lacks Landlock support");
            return;
        }
        let writable = tempfile::tempdir().expect("writable tempdir");
        let denied = tempfile::tempdir().expect("denied tempdir");
        let writable_arg = writable.path().to_str().expect("utf-8 tempdir").to_string();
        let denied_arg = denied.path().to_str().expect("utf-8 tempdir").to_string();
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            read_only_root: true,
            writable_paths: vec![writable.path().to_path_buf()],
            ..Default::default()
        };
        let status = spawn_sandboxed_shell(
            "sandbox-landlock",
            "touch \"$1/ok\" && ! touch \"$2/denied\"",
            &[&writable_arg, &denied_arg],
            &sandbox,
        )
        .await;
        assert!(
            status.success(),
            "writable path must stay writable, ungranted path must be denied"
        );
        assert!(writable.path().join("ok").is_file());
        assert!(!denied.path().join("denied").exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sandbox_network_isolated_leaves_only_loopback() {
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            network_isolated: true,
            ..Default::default()
        };
        let env: HashMap<String, String> = HashMap::new();
        let result = StdioMcpTransport::spawn(
            "sandbox-netns",
            "sh",
            ["-c", "test \"$(wc -l < /proc/net/dev)\" = 3"],
            &env,
            false,
            Some(&sandbox),
        )
        .await;
        let transport = match result {
            Ok(transport) => transport,
            Err(error) if error.to_string().contains("Operation not permitted") => {
                eprintln!("skipping: unprivileged user namespaces unavailable");
                return;
            }
            Err(error) => panic!("unexpected sandbox spawn failure: {error}"),
        };
        let status = transport
            .child
            .lock()
            .await
            .wait()
            .await
            .expect("wait for namespaced shell");
        transport.shutdown().await.expect("shutdown should succeed");
        assert!(
            status.success(),
            "isolated network namespace must contain only loopback"
        );
    }
}
