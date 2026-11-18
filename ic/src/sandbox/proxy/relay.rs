//! Per-execution Unix transport to the host's loopback HTTP proxy.
//!
//! Enforced containers retain `--network none`; legacy Podman containers keep
//! their normal advisory network. Only this socket's directory is mounted for
//! proxy transport; neither the container runtime socket nor the host proxy's
//! TCP listener is exposed to the container.
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
//!
//! # SELinux (WI#641)
//!
//! The runtime applies the shared `:z` label to the channel and socket inode.
//! Inode permissions (`sock_file`) and permission to reach the listening
//! socket (`unix_stream_socket connectto`) are separate SELinux checks. An
//! inode relabel does not change the listening socket's context. The cause of
//! the reported `EACCES` needs host audit evidence; do not change policy here.

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
            reason: "the proxy relay requires a local Linux container runtime".into(),
        })
    }

    pub fn path(&self) -> &Path {
        // The parent directory is retained until the execution supervisor ends.
        &self.channel
    }
}

/// Add path context to an I/O failure; a bare errno cannot say which of the
/// four host-side nodes failed.
#[cfg(target_os = "linux")]
fn at_path(context: &str, path: &Path, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        format!("{context} {}: {error}", path.display()),
    )
}

/// Build the isolated relay socket directory: a unique private parent dir, a
/// world-traversable `channel` child (only it is bind-mounted, so a rootless
/// keep-id user and a Docker-spawned UID can both traverse it), and a
/// world-connectable socket inside it.
#[cfg(target_os = "linux")]
fn relay_setup() -> std::io::Result<(tempfile::TempDir, PathBuf, tokio::net::UnixListener)> {
    use std::os::unix::fs::PermissionsExt;

    let base = relay_parent_dir()?;
    let directory = tempfile::Builder::new()
        .prefix("lw-proxy-")
        .tempdir_in(&base)
        .map_err(|error| at_path("cannot create relay directory in", &base, error))?;
    // `tempfile` creates at 0777 & ~umask; only the `channel` child is meant to
    // be reachable by another uid, so the parent stays private.
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .map_err(|error| at_path("cannot set relay directory mode", directory.path(), error))?;
    let channel = directory.path().join("channel");
    std::fs::create_dir(&channel)
        .map_err(|error| at_path("cannot create relay channel", &channel, error))?;
    std::fs::set_permissions(&channel, std::fs::Permissions::from_mode(0o711))
        .map_err(|error| at_path("cannot set relay channel mode", &channel, error))?;
    let socket = channel.join("proxy.sock");
    let listener = tokio::net::UnixListener::bind(&socket)
        .map_err(|error| at_path("cannot bind relay socket", &socket, error))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))
        .map_err(|error| at_path("cannot set relay socket mode", &socket, error))?;
    // A host connect would queue an upstream client without testing the
    // container's mount, UID mapping or security context.
    tracing::debug!(
        socket = %socket.display(),
        channel_mode = %format!("{:04o}", mode_of(&channel)),
        socket_mode = %format!("{:04o}", mode_of(&socket)),
        "proxy relay ready"
    );
    Ok((directory, channel, listener))
}

#[cfg(target_os = "linux")]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o7777)
        .unwrap_or(0)
}

/// Preferred parent for relay sockets: `$XDG_RUNTIME_DIR` (shared per-user,
/// never private-tmp'd) when it exists and belongs to our euid. `/run/user/<uid>`
/// is shared across the user manager's mount namespaces — daemon,
/// transient-service workloads, and rootless podman state — so relay sockets
/// remain resolvable even when the daemon runs with `PrivateTmp=true` and the
/// container CLI runs in a different mount namespace (manager-forked transient
/// services). Falls back to the process temp dir otherwise.
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
    use std::os::unix::fs::PermissionsExt;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

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

    /// WI#641: the container connects as an unmapped UID, so the channel must
    /// stay traversable and the socket connectable under DAC regardless of
    /// its owner, without broadening access to the private host parent.
    #[tokio::test]
    async fn relay_channel_is_world_traversable_and_socket_world_connectable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = ProxyRelay::start(listener.local_addr().unwrap().port())
            .await
            .unwrap();
        let channel = relay.path();
        let socket = channel.join("proxy.sock");
        assert_eq!(
            std::fs::metadata(channel).unwrap().permissions().mode() & 0o777,
            0o711
        );
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o7777,
            0o666
        );
        // The private parent keeps the socket reachable for this user only;
        // the channel itself is the sole mounted node.
        let parent = channel.parent().unwrap();
        assert_eq!(
            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
