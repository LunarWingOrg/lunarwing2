//! NetBird Reverse Proxy tunnel via `netbird expose`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

use crate::tunnel::{
    SharedProcess, SharedUrl, Tunnel, TunnelProcess, new_shared_process, new_shared_url,
    terminate_child, terminate_shared,
};

const READINESS_TIMEOUT: Duration = Duration::from_secs(20);
const READINESS_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const READINESS_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const EXPOSE_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(7);
const MAX_ERROR_LINES: usize = 8;
const MAX_ERROR_LINE_CHARS: usize = 512;

/// Exposes one local HTTP port through a connected NetBird peer.
///
/// Peer enrollment and daemon lifecycle remain operator-managed. LunarWing only
/// owns the ephemeral `netbird expose` process created by this instance.
pub struct NetBirdTunnel {
    custom_domain: Option<String>,
    name_prefix: String,
    binary: PathBuf,
    proc: SharedProcess,
    url: SharedUrl,
}

impl NetBirdTunnel {
    pub fn new(custom_domain: Option<String>, name_prefix: String) -> Result<Self> {
        validate_name_prefix(&name_prefix)?;
        let custom_domain = custom_domain
            .map(|domain| domain.trim().to_string())
            .filter(|domain| !domain.is_empty());

        Ok(Self {
            custom_domain,
            name_prefix,
            binary: PathBuf::from("netbird"),
            proc: new_shared_process(),
            url: new_shared_url(),
        })
    }

    #[cfg(test)]
    fn with_binary(
        custom_domain: Option<String>,
        name_prefix: String,
        binary: PathBuf,
    ) -> Result<Self> {
        let mut tunnel = Self::new(custom_domain, name_prefix)?;
        tunnel.binary = binary;
        Ok(tunnel)
    }

    async fn wait_until_ready(&self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
        let mut detail: String;

        loop {
            let result = tokio::time::timeout(
                READINESS_COMMAND_TIMEOUT,
                Command::new(&self.binary)
                    .args(["status", "--check", "startup"])
                    .kill_on_drop(true)
                    .output(),
            )
            .await;

            match result {
                Ok(Ok(output)) if output.status.success() => return Ok(()),
                Ok(Ok(output)) => detail = output_detail(&output),
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(error).with_context(|| {
                        format!("netbird binary not found at {}", self.binary.display())
                    });
                }
                Ok(Err(error)) => detail = error.to_string(),
                Err(_) => detail = "netbird status --check startup timed out".to_string(),
            }

            if tokio::time::Instant::now() >= deadline {
                bail!("NetBird client did not become ready within 20s: {detail}");
            }
            tokio::time::sleep(READINESS_RETRY_INTERVAL).await;
        }
    }

    fn expose_args(&self, local_port: u16) -> Vec<String> {
        let mut args = vec![
            "expose".to_string(),
            local_port.to_string(),
            "--protocol".to_string(),
            "http".to_string(),
            "--with-name-prefix".to_string(),
            self.name_prefix.clone(),
        ];
        if let Some(domain) = &self.custom_domain {
            args.extend(["--with-custom-domain".to_string(), domain.clone()]);
        }
        args
    }
}

#[async_trait::async_trait]
impl Tunnel for NetBirdTunnel {
    fn name(&self) -> &str {
        "netbird"
    }

    async fn start(&self, local_host: &str, local_port: u16) -> Result<String> {
        validate_local_host(local_host)?;
        if self.proc.lock().await.is_some() {
            bail!("NetBird tunnel is already running");
        }
        self.wait_until_ready().await?;

        let mut command = Command::new(&self.binary);
        command
            .args(self.expose_args(local_port))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        configure_parent_death_signal(&mut command);

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start {} expose", self.binary.display()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture netbird expose stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture netbird expose stderr"))?;
        let (stderr_drain, recent_stderr) = drain_stderr(stderr);

        let (public_url, mut stdout_lines) =
            match wait_for_public_url(&mut child, stdout, &recent_stderr).await {
                Ok(result) => result,
                Err(error) => {
                    terminate_child(&mut child, SHUTDOWN_TIMEOUT).await.ok();
                    stderr_drain.await.ok();
                    return Err(error);
                }
            };

        let drain_handle = tokio::spawn(async move {
            while let Ok(Some(line)) = stdout_lines.next_line().await {
                tracing::trace!("netbird expose: {line}");
            }
            stderr_drain.await.ok();
        });

        let mut guard = self.proc.lock().await;
        if guard.is_some() {
            terminate_child(&mut child, SHUTDOWN_TIMEOUT).await.ok();
            bail!("NetBird tunnel was started concurrently");
        }
        *guard = Some(TunnelProcess {
            child,
            _pipe_drain: Some(drain_handle),
        });
        if let Ok(mut url) = self.url.write() {
            *url = Some(public_url.clone());
        }
        Ok(public_url)
    }

    async fn stop(&self) -> Result<()> {
        let result = terminate_shared(&self.proc, SHUTDOWN_TIMEOUT).await;
        if let Ok(mut url) = self.url.write() {
            *url = None;
        }
        result
    }

    async fn health_check(&self) -> bool {
        if self.url.read().ok().is_none_or(|url| url.is_none()) {
            return false;
        }
        let mut guard = self.proc.lock().await;
        guard.as_mut().is_some_and(|process| {
            process
                .child
                .try_wait()
                .is_ok_and(|status| status.is_none())
        })
    }

    fn public_url(&self) -> Option<String> {
        self.url.read().ok().and_then(|url| url.clone())
    }
}

#[cfg(target_os = "linux")]
fn configure_parent_death_signal(command: &mut Command) {
    let parent_pid = nix::unistd::getpid();
    // SAFETY: Callback only invokes async-signal-safe syscalls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGTERM)
                .map_err(std::io::Error::from)?;
            if nix::unistd::getppid() != parent_pid {
                return Err(std::io::Error::from_raw_os_error(nix::libc::ESRCH));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_parent_death_signal(_command: &mut Command) {}

pub(crate) fn validate_name_prefix(prefix: &str) -> Result<()> {
    let bytes = prefix.as_bytes();
    let valid_edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let valid_body = |byte: &u8| valid_edge(*byte) || *byte == b'-';

    if !(1..=32).contains(&bytes.len())
        || !bytes.iter().all(valid_body)
        || !valid_edge(bytes[0])
        || !valid_edge(bytes[bytes.len() - 1])
    {
        bail!(
            "NetBird name prefix must be 1-32 lowercase letters, digits, or hyphens, and start and end with a letter or digit"
        );
    }
    Ok(())
}

fn validate_local_host(host: &str) -> Result<()> {
    if matches!(host, "127.0.0.1" | "localhost" | "0.0.0.0") {
        return Ok(());
    }
    bail!(
        "NetBird expose forwards to IPv4 loopback; local listener host must be 127.0.0.1, localhost, or 0.0.0.0 (got {host})"
    )
}

fn extract_public_url(line: &str) -> Option<String> {
    let value = line.trim().strip_prefix("URL:")?.trim();
    let candidate = value.split_whitespace().next()?;
    let parsed = url::Url::parse(candidate).ok()?;
    (parsed.scheme() == "https" && parsed.host_str().is_some()).then(|| candidate.to_string())
}

async fn wait_for_public_url(
    child: &mut Child,
    stdout: ChildStdout,
    recent_stderr: &Arc<StdMutex<VecDeque<String>>>,
) -> Result<(String, Lines<BufReader<ChildStdout>>)> {
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::Instant::now() + EXPOSE_TIMEOUT;

    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "netbird expose did not produce a public HTTPS URL within 60s: {}",
                captured_error(recent_stderr)
            );
        }

        match tokio::time::timeout(Duration::from_secs(1), lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                tracing::trace!("netbird expose: {line}");
                if let Some(url) = extract_public_url(&line) {
                    return Ok((url, lines));
                }
            }
            Ok(Ok(None)) => tokio::time::sleep(Duration::from_millis(50)).await,
            Ok(Err(error)) => bail!("failed reading netbird expose output: {error}"),
            Err(_) => {}
        }

        if let Some(status) = child.try_wait()? {
            bail!(
                "netbird expose exited with {status} before publishing a URL: {}",
                captured_error(recent_stderr)
            );
        }
    }
}

fn drain_stderr(
    stderr: tokio::process::ChildStderr,
) -> (tokio::task::JoinHandle<()>, Arc<StdMutex<VecDeque<String>>>) {
    let recent = Arc::new(StdMutex::new(VecDeque::with_capacity(MAX_ERROR_LINES)));
    let captured = Arc::clone(&recent);
    let handle = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::trace!("netbird expose: {line}");
            let line: String = line.chars().take(MAX_ERROR_LINE_CHARS).collect();
            let mut guard = captured.lock().unwrap_or_else(|error| error.into_inner());
            if guard.len() == MAX_ERROR_LINES {
                guard.pop_front();
            }
            guard.push_back(line);
        }
    });
    (handle, recent)
}

fn captured_error(lines: &Arc<StdMutex<VecDeque<String>>>) -> String {
    let guard = lines.lock().unwrap_or_else(|error| error.into_inner());
    if guard.is_empty() {
        "no error detail".to_string()
    } else {
        guard.iter().cloned().collect::<Vec<_>>().join(" | ")
    }
}

fn output_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    stderr
        .lines()
        .chain(stdout.lines())
        .find(|line| !line.trim().is_empty())
        .map(|line| line.chars().take(MAX_ERROR_LINE_CHARS).collect())
        .unwrap_or_else(|| format!("netbird status exited with {}", output.status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_labelled_https_url() {
        assert_eq!(
            extract_public_url("  URL:      https://lw.proxy.example.com"),
            Some("https://lw.proxy.example.com".to_string())
        );
        assert!(extract_public_url("docs: https://docs.netbird.io").is_none());
        assert!(extract_public_url("URL: http://insecure.example.com").is_none());
    }

    #[test]
    fn validates_name_prefix() {
        assert!(validate_name_prefix("lunarwing-2").is_ok());
        assert!(validate_name_prefix("Uppercase").is_err());
        assert!(validate_name_prefix("-lunarwing").is_err());
        assert!(validate_name_prefix(&"x".repeat(33)).is_err());
    }

    #[test]
    fn builds_expose_arguments() {
        let tunnel = NetBirdTunnel::new(
            Some("lunarwing.example.com".to_string()),
            "lunarwing".to_string(),
        )
        .unwrap();
        assert_eq!(
            tunnel.expose_args(8080),
            [
                "expose",
                "8080",
                "--protocol",
                "http",
                "--with-name-prefix",
                "lunarwing",
                "--with-custom-domain",
                "lunarwing.example.com",
            ]
        );
    }

    #[test]
    fn rejects_non_loopback_specific_bind() {
        assert!(validate_local_host("127.0.0.1").is_ok());
        assert!(validate_local_host("0.0.0.0").is_ok());
        assert!(validate_local_host("10.0.0.5").is_err());
        assert!(validate_local_host("::").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn starts_checks_and_stops_fake_expose_process() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("netbird");
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = status ]; then exit 0; fi\nif [ \"$1\" = expose ]; then\n  printf 'Service exposed successfully!\\n  URL:      https://lunarwing.proxy.example.com\\n'\n  trap 'exit 0' TERM INT\n  while :; do sleep 1; done\nfi\nexit 2\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();

        let tunnel = NetBirdTunnel::with_binary(None, "lunarwing".to_string(), binary).unwrap();
        let url = tunnel.start("127.0.0.1", 8080).await.unwrap();
        assert_eq!(url, "https://lunarwing.proxy.example.com");
        assert!(tunnel.health_check().await);

        tunnel.stop().await.unwrap();
        assert!(tunnel.public_url().is_none());
        assert!(!tunnel.health_check().await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reports_expose_process_exit_before_url() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("netbird");
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = status ]; then exit 0; fi\necho 'permission denied' >&2\nexit 7\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();

        let tunnel = NetBirdTunnel::with_binary(None, "lunarwing".to_string(), binary).unwrap();
        let error = tunnel.start("127.0.0.1", 8080).await.unwrap_err();
        assert!(error.to_string().contains("exited with"));
        assert!(tunnel.public_url().is_none());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn parent_death_helper() {
        let Ok(pid_path) = std::env::var("LUNARWING_PDEATHSIG_HELPER") else {
            return;
        };

        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        configure_parent_death_signal(&mut command);
        let child = command.spawn().unwrap();
        let pid = child.id().unwrap() as i32;
        let (_, start_time) = linux_process_info(pid).unwrap();
        std::fs::write(pid_path, format!("{pid} {start_time}")).unwrap();
        std::process::exit(0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_death_signal_stops_expose_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("child.pid");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tunnel::netbird::tests::parent_death_helper",
                "--nocapture",
            ])
            .env("LUNARWING_PDEATHSIG_HELPER", &pid_path)
            .status()
            .unwrap();
        assert!(status.success());

        let identity = std::fs::read_to_string(pid_path).unwrap();
        let mut identity = identity.split_whitespace();
        let pid: i32 = identity.next().unwrap().parse().unwrap();
        let expected_start_time: u64 = identity.next().unwrap().parse().unwrap();
        for _ in 0..30 {
            match linux_process_info(pid) {
                None => return,
                Some((_, start_time)) if start_time != expected_start_time => return,
                Some((state, _)) if state == "Z" => return,
                Some(_) => {}
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        if linux_process_info(pid).is_some_and(|(_, start_time)| start_time == expected_start_time)
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        panic!("child {pid} survived parent exit");
    }

    #[cfg(target_os = "linux")]
    fn linux_process_info(pid: i32) -> Option<(String, u64)> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let mut fields = stat.rsplit_once(") ")?.1.split_whitespace();
        let state = fields.next()?.to_string();
        let start_time = fields.nth(18)?.parse().ok()?;
        Some((state, start_time))
    }

    #[test]
    fn binary_path_is_operator_command_by_default() {
        let tunnel = NetBirdTunnel::new(None, "lunarwing".to_string()).unwrap();
        assert_eq!(tunnel.binary, std::path::Path::new("netbird"));
    }
}
