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
//! A socket the host created is only reachable from the confined container
//! when its inode carries a type the container domain can `connectto` — in
//! practice `container_file_t` — and a level no container's MCS pair has to
//! dominate. The engine's `:z` relabel produces exactly that, but a relabel
//! the engine skips or cannot apply leaves the socket at its host type and
//! every `connect()` fails with `EACCES`, which is what the tenant saw. When
//! SELinux is enforcing we therefore try to set the type ourselves and verify
//! it by read-back; only a verified label lets the caller drop `:z` (a second
//! relabel would pin the socket to one container's category pair). Every
//! failure path keeps `:z`, so non-SELinux hosts and unprivileged daemons
//! behave exactly as before.

use std::path::{Path, PathBuf};

use crate::sandbox::error::{Result, SandboxError};

pub(crate) struct ProxyRelay {
    _directory: tempfile::TempDir,
    channel: PathBuf,
    /// Set when [`relay_setup`] proved that the mounted nodes already carry a
    /// container-readable SELinux type and category-free level.
    container_label_ready: bool,
    task: tokio::task::JoinHandle<()>,
}

impl ProxyRelay {
    #[cfg(target_os = "linux")]
    pub async fn start(port: u16) -> Result<Self> {
        let (directory, channel, listener, container_label_ready) =
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
            container_label_ready,
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

    /// Whether the bind mount must still ask the container engine for `:z`.
    ///
    /// `false` only when the relay directory already carries a verified
    /// container-readable SELinux label, where a second relabel would replace
    /// it with one container's MCS pair and lock every other execution out.
    pub fn mount_relabel(&self) -> bool {
        !self.container_label_ready
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
fn relay_setup() -> std::io::Result<(tempfile::TempDir, PathBuf, tokio::net::UnixListener, bool)> {
    use std::os::unix::fs::PermissionsExt;

    let base = relay_parent_dir()?;
    let directory = tempfile::Builder::new()
        .prefix("lw-proxy-")
        .tempdir_in(&base)
        .map_err(|error| at_path("cannot create relay directory in", &base, error))?;
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
    // A socket that cannot be connected to locally can never be connected to
    // from the container; fail before the command is launched.
    std::os::unix::net::UnixStream::connect(&socket)
        .map_err(|error| at_path("relay socket refused its own probe", &socket, error))?;
    let container_label_ready = label_for_containers(&channel, &socket);
    tracing::debug!(
        socket = %socket.display(),
        channel_mode = %format!("{:04o}", mode_of(&channel)),
        socket_mode = %format!("{:04o}", mode_of(&socket)),
        container_label_ready = container_label_ready,
        "proxy relay ready"
    );
    Ok((directory, channel, listener, container_label_ready))
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

/// Is SELinux enforcing on this host? Absent `/sys/fs/selinux` means no.
#[cfg(target_os = "linux")]
fn selinux_enforcing() -> bool {
    std::fs::read_to_string("/sys/fs/selinux/enforce")
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
}

/// The `security.selinux` context of one node, or `None` when unlabelled.
#[cfg(target_os = "linux")]
fn selinux_context(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buffer = [0u8; 256];
    // SAFETY: the path and attribute name are valid NUL-terminated strings,
    // and the buffer pointer/length describe the same live allocation.
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"security.selinux".as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if read <= 0 {
        return None;
    }
    let bytes = &buffer[..read as usize];
    let text = std::str::from_utf8(bytes).ok()?.trim_end_matches('\0');
    Some(text.to_string())
}

#[cfg(target_os = "linux")]
fn write_selinux_context(path: &Path, context: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let Ok(value) = std::ffi::CString::new(context) else {
        return false;
    };
    // SAFETY: both pointers are valid NUL-terminated strings for the call and
    // the size excludes the terminator.
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            c"security.selinux".as_ptr(),
            value.as_ptr().cast(),
            value.as_bytes().len(),
            0,
        ) == 0
    }
}

/// Replace the type field of a context, keeping user, role and level.
#[cfg(target_os = "linux")]
fn with_container_file_type(context: &str) -> Option<String> {
    let mut fields = context.splitn(4, ':');
    let user = fields.next()?;
    let role = fields.next()?;
    let _type = fields.next()?;
    let level = fields.next()?;
    if user.is_empty() || role.is_empty() || level.is_empty() {
        return None;
    }
    Some(format!("{user}:{role}:container_file_t:{level}"))
}

/// Labels every container's MCS pair dominates. A category-carrying level is
/// only reachable from one container, so it must not count as ready.
#[cfg(target_os = "linux")]
fn level_is_category_free(level: &str) -> bool {
    matches!(level, "s0" | "s0-s0")
}

/// Give the relay nodes the shared container type so `connect()` does not
/// depend on the engine relabeling a socket it may treat as already labelled.
///
/// `true` only when a read-back proves both nodes carry `container_file_t`
/// with a category-free level. Any failure leaves the caller on the engine's
/// `:z` relabel, i.e. the previous behaviour (WI#641).
#[cfg(target_os = "linux")]
fn label_for_containers(channel: &Path, socket: &Path) -> bool {
    if !selinux_enforcing() {
        return false;
    }
    for path in [channel, socket] {
        let Some(current) = selinux_context(path) else {
            return false;
        };
        let Some(updated) = with_container_file_type(&current) else {
            return false;
        };
        if !write_selinux_context(path, &updated) {
            return false;
        }
        let Some(verified) = selinux_context(path) else {
            return false;
        };
        let mut fields = verified.splitn(4, ':');
        let (Some(_user), Some(_role), Some(kind), Some(level)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        if kind != "container_file_t" || !level_is_category_free(level) {
            return false;
        }
    }
    true
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
    /// stay traversable and the socket connectable regardless of its owner,
    /// and only an unverified SELinux label may request an engine relabel.
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
        if !selinux_enforcing() {
            assert!(
                relay.mount_relabel(),
                "an unlabelled host must keep the engine's :z relabel"
            );
        }
    }

    #[test]
    fn container_type_replaces_only_the_type_field() {
        assert_eq!(
            with_container_file_type("unconfined_u:object_r:user_tmp_t:s0").as_deref(),
            Some("unconfined_u:object_r:container_file_t:s0")
        );
        assert_eq!(
            with_container_file_type("system_u:object_r:tmp_t:s0:c1,c2").as_deref(),
            Some("system_u:object_r:container_file_t:s0:c1,c2")
        );
        assert_eq!(with_container_file_type("broken"), None);
        assert_eq!(with_container_file_type("u:r:t:"), None);
    }

    #[test]
    fn only_a_category_free_level_counts_as_container_ready() {
        assert!(level_is_category_free("s0"));
        assert!(level_is_category_free("s0-s0"));
        assert!(!level_is_category_free("s0:c1,c2"));
        assert!(!level_is_category_free("s0-s0:c0.c1023"));
    }
}
