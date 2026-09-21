//! Per-execution Unix transport to the host's loopback HTTP proxy.
//!
//! The container retains `--network none`. Only this socket's directory is
//! mounted; neither the container runtime socket nor any host TCP listener is
//! exposed.
//!
//! # Socket placement (!1044 review finding 1)
//!
//! The socket path must be resolvable by **both** the daemon process and the
//! container runtime's mount namespace. The daemon may run under a systemd
//! unit with `PrivateTmp=true`, and on multi-tenant hosts the workload
//! spawner runs the container CLI from a manager-forked transient service in
//! a *different* mount namespace. A socket created under the daemon's private
//! `/tmp` would then be invisible at `podman/docker create` time ("statfs
//! ...: no such file or directory"). `$XDG_RUNTIME_DIR` (`/run/user/<uid>`)
//! is shared per-user and never private-tmp'd, so it is the preferred parent.
//! The process temp dir remains the fallback for contexts where the runtime
//! dir does not exist or is not ours.

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
        let (directory, channel, listener) =
            relay_setup().map_err(|e| SandboxError::ProxyError {
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
                }
            }
            // Task abort aborts every open connection, including CONNECT.
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
        // The parent directory is retained until the execution supervisor ends.
        &self.channel
    }
}

/// Build the isolated relay socket directory: a unique private parent dir, a
/// world-traversable `channel` child (only it is bind-mounted, so a rootless
/// keep-id user and a Docker-spawned UID can both traverse it), and a
/// world-connectable socket inside it.
fn relay_setup() -> std::io::Result<(tempfile::TempDir, PathBuf, tokio::net::UnixListener)> {
    use std::os::unix::fs::PermissionsExt;

    let base = relay_parent_dir()?;
    let directory = tempfile::Builder::new()
        .prefix("lw-proxy-")
        .tempdir_in(base)?;
    let channel = directory.path().join("channel");
    std::fs::create_dir(&channel)?;
    std::fs::set_permissions(&channel, std::fs::Permissions::from_mode(0o711))?;
    let socket = channel.join("proxy.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))?;
    Ok((directory, channel, listener))
}

/// Preferred parent for relay sockets: `$XDG_RUNTIME_DIR/lunarwing-sandbox`
/// (mode 0700, belonging to our euid) when the runtime dir exists and belongs
/// to our euid. `/run/user/<uid>` is shared across the user manager's mount
/// namespaces — daemon, transient-service workloads, and rootless podman
/// state — so relay sockets remain resolvable even when the daemon runs with
/// `PrivateTmp=true` and the container CLI runs in a different mount
/// namespace (manager-forked transient services). Falls back to the process
/// temp dir otherwise.
#[cfg(target_os = "linux")]
pub(crate) fn relay_parent_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(&xdg);
        if dir.is_absolute()
            && let Ok(meta) = std::fs::metadata(&dir)
            && meta.is_dir()
            && meta.uid() == nix::unistd::geteuid().as_raw()
        {
            return Ok(dir);
        }
    }
    Ok(std::env::temp_dir())
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
        let local = relay.path().join("proxy.sock");
        drop(relay);
        assert!(!local.exists());
        let read = tokio::time::timeout(std::time::Duration::from_secs(2), client.read(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, 0, "relay cancellation must close active streams");
    }
}
