//! Per-execution Unix transport to the host's loopback HTTP proxy.
//!
//! The container retains `--network none`. Only this socket's directory is
//! mounted; neither the runtime socket nor any host TCP listener is exposed.

use std::path::{Path, PathBuf};

use crate::sandbox::error::{Result, SandboxError};

pub(crate) struct ProxyRelay {
    _directory: tempfile::TempDir,
    channel: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl ProxyRelay {
    #[cfg(target_os = "linux")]
    pub async fn start(port: u16) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let setup = || -> std::io::Result<_> {
            // A private parent prevents other host users from reaching the
            // world-connectable socket. Only its child is bind-mounted, so
            // Docker's workspace UID and Podman's keep-id can both connect.
            let directory = tempfile::Builder::new().prefix("lw-proxy-").tempdir()?;
            let channel = directory.path().join("channel");
            std::fs::create_dir(&channel)?;
            std::fs::set_permissions(&channel, std::fs::Permissions::from_mode(0o711))?;
            let socket = channel.join("proxy.sock");
            let listener = tokio::net::UnixListener::bind(&socket)?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))?;
            Ok((directory, channel, listener))
        };
        let (directory, channel, listener) = setup().map_err(|e| SandboxError::ProxyError {
            reason: format!("cannot create isolated proxy transport: {e}"),
        })?;
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept(), if connections.len() < 32 => {
                        let Ok((mut stream, _)) = accepted else { break };
                        connections.spawn(async move {
                            let _ = tokio::time::timeout(std::time::Duration::from_secs(120), async {
                                let mut proxy = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
                                tokio::io::copy_bidirectional(&mut stream, &mut proxy).await
                            }).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            // JoinSet drop aborts every open connection, including CONNECT.
        });
        Ok(Self {
            _directory: directory,
            channel,
            task,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn start(_port: u16) -> Result<Self> {
        Err(SandboxError::Config {
            reason: "proxy-enforced requires a local Linux container runtime".into(),
        })
    }

    pub fn path(&self) -> &Path {
        // The tempfile parent is retained until the execution supervisor ends.
        &self.channel
    }
}

impl Drop for ProxyRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn relay_drop_removes_socket_and_closes_live_connections() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = ProxyRelay::start(listener.local_addr().unwrap().port())
            .await
            .unwrap();
        let path = relay.path().join("proxy.sock");
        let mut client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let (mut upstream, _) = listener.accept().await.unwrap();
        upstream.write_all(b"hello").await.unwrap();
        let mut bytes = [0; 5];
        client.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"hello");
        drop(relay);
        assert!(!path.exists());
        let read = tokio::time::timeout(std::time::Duration::from_secs(2), client.read(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, 0, "relay cancellation must close active streams");
    }

    /// Uses the host's real network namespace isolation when available. This
    /// verifies the transport without pretending it is container-runtime E2E.
    #[tokio::test]
    async fn isolated_namespace_can_only_egress_through_allowlist_proxy() {
        let probe = tokio::process::Command::new("bwrap")
            .args([
                "--unshare-user",
                "--unshare-net",
                "--ro-bind",
                "/",
                "/",
                "true",
            ])
            .output()
            .await;
        if !probe.is_ok_and(|o| o.status.success()) {
            eprintln!("SKIP network namespace acceptance: bwrap/user namespaces unavailable");
            return;
        }
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = upstream.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut data = [0; 4096];
            assert!(socket.read(&mut data).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nrelay-works!").await.unwrap();
        });
        // Local upstream is deliberately allowed by this fixture proxy. The
        // production enforced proxy separately rejects even allowlisted LAN IPs.
        let proxy = crate::sandbox::proxy::NetworkProxyBuilder::new()
            .with_allowlist(vec!["127.0.0.1".into()])
            .with_credentials(vec![])
            .build_and_start(0)
            .await
            .unwrap();
        let relay = ProxyRelay::start(proxy.addr().await.unwrap().port())
            .await
            .unwrap();
        let command = format!(
            "set -e; curl --noproxy '' -fsS --max-time 5 -x http://127.0.0.1:3128 http://{target}/; \
             if curl --noproxy '*' -fsS --max-time 1 http://{target}/; then exit 91; fi; \
             if curl --noproxy '*' -fsS --max-time 1 http://1.1.1.1/; then exit 92; fi; \
             curl --noproxy '' -sS --max-time 5 -x http://127.0.0.1:3128 http://denied.invalid/"
        );
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            tokio::process::Command::new("bwrap")
                .args([
                    "--unshare-user",
                    "--unshare-net",
                    "--ro-bind",
                    "/",
                    "/",
                    "--tmpfs",
                    "/run",
                    "--ro-bind",
                ])
                .arg(relay.path())
                .arg("/run/lunarwing-proxy")
                .args(["python3", "-I", "-c", include_str!("relay.py"), &command])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("relay-works!"), "{text}");
        assert!(text.contains("sandbox-proxy denied:"), "{text}");
        server.await.unwrap();
        proxy.stop().await;
    }
}
